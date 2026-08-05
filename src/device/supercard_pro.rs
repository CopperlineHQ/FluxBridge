// SPDX-License-Identifier: MPL-2.0 AND LGPL-3.0-or-later

//! SuperCard Pro serial protocol.

use std::time::{Duration, Instant};

use crate::device::{Device, RawCapture, operation_deadline, validate_capture};
use crate::flux::{FluxEvent, flux_to_revolution, flux_to_unaligned_revolution, mfm_to_flux};
use crate::transport::{self, Transport, read_exact_until, write_all_until};
use crate::{
    BridgeConfig, DensityMode, DriveSelect, DriveStatus, DriveType, Error, PortId, ReadMode,
    Result, Side,
};

const BAUD: u32 = 9_600;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const STREAM_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_STREAM_BYTES: usize = 1 << 20;
const RESPONSE_OK: u8 = 0x4f;
const RESPONSE_WRITE_PROTECTED: u8 = 0x0f;
const RESPONSE_NO_DISK: u8 = 0x11;
const RESPONSE_NOT_READY: u8 = 0x08;
const RESPONSE_OVERRUN: u8 = 0x15;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    SelectA = 0x80,
    SelectB = 0x81,
    DeselectA = 0x82,
    DeselectB = 0x83,
    MotorAOn = 0x84,
    MotorBOn = 0x85,
    MotorAOff = 0x86,
    MotorBOff = 0x87,
    SeekZero = 0x88,
    StepTo = 0x89,
    StepOut = 0x8b,
    Side = 0x8d,
    Status = 0x8e,
    SetParams = 0x91,
    WriteFlux = 0xa2,
    LoadRamUsb = 0xaa,
    StartStream = 0xae,
    StopStream = 0xaf,
    Info = 0xd0,
}

/// Open SuperCard Pro device.
pub(super) struct SuperCardPro {
    transport: Box<dyn Transport>,
    port: PortId,
    status: DriveStatus,
    drive_a: bool,
    selected: bool,
    high_density: bool,
}

impl SuperCardPro {
    pub(super) fn open(port: PortId, config: &BridgeConfig) -> Result<Box<dyn Device>> {
        let drive_a = match config.drive {
            DriveSelect::PcA => true,
            DriveSelect::PcB => false,
            _ => {
                return Err(Error::InvalidConfig(
                    "SuperCard Pro supports only PC Drive A or Drive B".into(),
                ));
            }
        };
        // Unlike the original implementation, the caller's exact port is
        // always the port opened here.
        let transport = transport::open(&port, BAUD, COMMAND_TIMEOUT)?;
        let mut device = Self {
            transport,
            port,
            status: DriveStatus {
                max_cylinders: 84,
                drive_type: DriveType::Hd35,
                ..DriveStatus::default()
            },
            drive_a,
            selected: false,
            high_density: config.density == DensityMode::High,
        };
        device.transport.purge()?;
        let info = device.command(Command::Info, &[], 2)?;
        let hardware = (info[0] >> 4, info[0] & 0x0f);
        let firmware = (info[1] >> 4, info[1] & 0x0f);
        if firmware < (1, 3) {
            return Err(Error::UnsupportedFirmware(format!(
                "SuperCard Pro firmware {}.{} on hardware {}.{}; firmware 1.3 or later is required",
                firmware.0, firmware.1, hardware.0, hardware.1
            )));
        }

        device.expect_ok(Command::MotorAOff, &[])?;
        device.expect_ok(Command::MotorBOff, &[])?;
        device.expect_ok(Command::DeselectA, &[])?;
        device.expect_ok(Command::DeselectB, &[])?;
        device.expect_ok(Command::SeekZero, &[])?;
        device.status.working = true;
        device.status.cylinder = 0;
        let _ = device.check_pins();
        Ok(Box::new(device))
    }

    fn packet(command: Command, payload: &[u8]) -> Result<Vec<u8>> {
        let payload_len = u8::try_from(payload.len())
            .map_err(|_| Error::InvalidConfig("SuperCard Pro command payload too long".into()))?;
        let mut packet = Vec::with_capacity(payload.len() + 3);
        packet.extend_from_slice(&[command as u8, payload_len]);
        packet.extend_from_slice(payload);
        packet.push(
            packet
                .iter()
                .fold(0x4a_u8, |sum, value| sum.wrapping_add(*value)),
        );
        Ok(packet)
    }

    fn raw_command(&mut self, command: Command, payload: &[u8]) -> Result<u8> {
        let packet = Self::packet(command, payload)?;
        let deadline = operation_deadline();
        write_all_until(
            self.transport.as_mut(),
            &packet,
            deadline,
            "SuperCard Pro command",
        )?;
        let mut response = [0_u8; 2];
        read_exact_until(
            self.transport.as_mut(),
            &mut response,
            deadline,
            "SuperCard Pro acknowledgement",
        )?;
        if response[0] != command as u8 {
            return Err(Error::Protocol(format!(
                "SuperCard Pro replied to command {:#04x} while {:#04x} was pending",
                response[0], command as u8
            )));
        }
        Ok(response[1])
    }

    fn command(&mut self, command: Command, payload: &[u8], extra: usize) -> Result<Vec<u8>> {
        let response = self.raw_command(command, payload)?;
        map_response(response)?;
        let mut output = vec![0_u8; extra];
        if extra != 0 {
            read_exact_until(
                self.transport.as_mut(),
                &mut output,
                operation_deadline(),
                "SuperCard Pro command payload",
            )?;
        }
        Ok(output)
    }

    fn expect_ok(&mut self, command: Command, payload: &[u8]) -> Result<()> {
        self.command(command, payload, 0).map(drop)
    }

    fn select(&mut self, selected: bool) -> Result<()> {
        if selected == self.selected {
            return Ok(());
        }
        let command = match (self.drive_a, selected) {
            (true, true) => Command::SelectA,
            (false, true) => Command::SelectB,
            (true, false) => Command::DeselectA,
            (false, false) => Command::DeselectB,
        };
        self.expect_ok(command, &[])?;
        self.selected = selected;
        Ok(())
    }

    fn check_pins(&mut self) -> Result<()> {
        let deselect = !self.status.motor_running;
        self.select(true)?;
        let bytes = self.command(Command::Status, &[], 2)?;
        let status = u16::from_be_bytes([bytes[0], bytes[1]]);
        self.status.write_protected = status & (1 << 7) == 0;
        self.status.disk_present = status & (1 << 6) != 0;
        if deselect {
            self.select(false)?;
        }
        Ok(())
    }

    fn read_stream(&mut self, mode: ReadMode) -> Result<RawCapture> {
        self.select(true)?;
        // Raw flux, 8-bit counters, 50ns base resolution. Compatible and
        // stalling reads request stream-from-index; fast reads start now.
        let flags = 0x04 | 0x02 | u8::from(mode != ReadMode::Fast);
        let response = self.raw_command(Command::StartStream, &[flags])?;
        match response {
            RESPONSE_OK => {}
            RESPONSE_NO_DISK | RESPONSE_NOT_READY => {
                self.status.disk_present = false;
                return Err(Error::Timeout("SuperCard Pro disk ready"));
            }
            other => map_response(other)?,
        }

        let deadline = Instant::now() + STREAM_TIMEOUT;
        let mut events = Vec::with_capacity(100_000);
        let mut pending_ticks = 0_u32;
        let mut pending_index = false;
        let mut saw_ff = false;
        let mut indices = 0_u8;
        let mut bytes_seen = 0_usize;
        let target_duration = if mode == ReadMode::Fast {
            260_000_000_u64
        } else {
            u64::MAX
        };
        let mut duration = 0_u64;

        loop {
            if bytes_seen >= MAX_STREAM_BYTES {
                return Err(Error::TrackTooLarge {
                    bits: bytes_seen * 8,
                    limit: MAX_STREAM_BYTES * 8,
                });
            }
            let mut byte = [0_u8];
            read_exact_until(
                self.transport.as_mut(),
                &mut byte,
                deadline,
                "SuperCard Pro flux stream",
            )?;
            bytes_seen += 1;
            let byte = byte[0];
            if saw_ff {
                saw_ff = false;
                if byte == 0 {
                    indices = indices.saturating_add(1);
                    if indices >= 2 && mode != ReadMode::Fast {
                        break;
                    }
                    pending_index = true;
                    continue;
                }
                pending_ticks = pending_ticks.saturating_add(u32::from(byte));
            } else if byte == 0xff {
                saw_ff = true;
                continue;
            } else if byte == 0 {
                pending_ticks = pending_ticks.saturating_add(256);
                continue;
            } else {
                pending_ticks = pending_ticks.saturating_add(u32::from(byte));
            }

            let resolution = if self.high_density { 100 } else { 50 };
            let nanoseconds = pending_ticks.saturating_mul(resolution);
            duration = duration.saturating_add(u64::from(nanoseconds));
            events.push(FluxEvent {
                nanoseconds,
                index: pending_index,
            });
            pending_index = false;
            pending_ticks = 0;
            if duration >= target_duration {
                break;
            }
        }

        let stop = Self::packet(Command::StopStream, &[])?;
        write_all_until(
            self.transport.as_mut(),
            &stop,
            deadline,
            "SuperCard Pro stop stream",
        )?;
        let mut window = [0_u8; 4];
        loop {
            let mut byte = [0_u8];
            read_exact_until(
                self.transport.as_mut(),
                &mut byte,
                deadline,
                "SuperCard Pro stop acknowledgement",
            )?;
            window.rotate_left(1);
            window[3] = byte[0];
            if window[..3] == [0xde, 0xad, Command::StopStream as u8] {
                match window[3] {
                    RESPONSE_OK => break,
                    RESPONSE_OVERRUN => {
                        return Err(Error::Protocol(
                            "SuperCard Pro stream buffer overrun".into(),
                        ));
                    }
                    other => map_response(other)?,
                }
            }
        }
        self.transport.purge()?;
        self.status.disk_present = true;
        self.status.ready = true;
        if !self.status.motor_running {
            self.select(false)?;
        }

        let (words, bit_len, index_aligned) = if mode == ReadMode::Fast {
            let (words, bit_len) = flux_to_unaligned_revolution(&events, self.high_density)?;
            (words, bit_len, false)
        } else {
            flux_to_revolution(&events)?
        };
        let capture = RawCapture {
            words,
            bit_len,
            index_aligned: mode != ReadMode::Fast && index_aligned,
        };
        validate_capture(&capture)?;
        Ok(capture)
    }

    fn write_stream(&mut self, words: &[u16], bit_len: usize, from_index: bool) -> Result<()> {
        let timings = mfm_to_flux(
            words,
            bit_len,
            if self.high_density { 1_000 } else { 2_000 },
        )?;
        let resolution = if self.high_density { 50 } else { 25 };
        let mut encoded = Vec::with_capacity(timings.len() * 2);
        let mut entries = 0_u32;
        for nanoseconds in timings {
            let mut ticks = (nanoseconds + resolution / 2) / resolution;
            while ticks > u32::from(u16::MAX) {
                encoded.extend_from_slice(&0_u16.to_be_bytes());
                entries = entries.saturating_add(1);
                ticks -= u32::from(u16::MAX) + 1;
            }
            encoded.extend_from_slice(
                &u16::try_from(ticks)
                    .expect("large tick values are split before conversion")
                    .to_be_bytes(),
            );
            entries = entries.saturating_add(1);
        }

        let mut header = Vec::with_capacity(8);
        header.extend_from_slice(&0_u32.to_be_bytes());
        header.extend_from_slice(
            &u32::try_from(encoded.len())
                .map_err(|_| Error::TrackTooLarge {
                    bits: bit_len,
                    limit: crate::flux::MAX_TRACK_BITS,
                })?
                .to_be_bytes(),
        );
        let packet = Self::packet(Command::LoadRamUsb, &header)?;
        let deadline = Instant::now() + STREAM_TIMEOUT;
        write_all_until(
            self.transport.as_mut(),
            &packet,
            deadline,
            "SuperCard Pro RAM load command",
        )?;
        write_all_until(
            self.transport.as_mut(),
            &encoded,
            deadline,
            "SuperCard Pro RAM load",
        )?;
        let mut response = [0_u8; 2];
        read_exact_until(
            self.transport.as_mut(),
            &mut response,
            deadline,
            "SuperCard Pro RAM load acknowledgement",
        )?;
        if response != [Command::LoadRamUsb as u8, RESPONSE_OK] {
            return Err(Error::Protocol(
                "SuperCard Pro rejected the flux RAM load".into(),
            ));
        }

        self.select(true)?;
        let mut payload = Vec::with_capacity(5);
        payload.extend_from_slice(&entries.to_be_bytes());
        payload.push(u8::from(from_index));
        match self.raw_command(Command::WriteFlux, &payload)? {
            RESPONSE_OK => Ok(()),
            RESPONSE_WRITE_PROTECTED => {
                self.status.write_protected = true;
                Err(Error::WriteProtected)
            }
            other => {
                map_response(other)?;
                Ok(())
            }
        }
    }
}

impl Device for SuperCardPro {
    fn selected_port(&self) -> &PortId {
        &self.port
    }

    fn status(&mut self) -> Result<DriveStatus> {
        self.check_pins()?;
        self.status.working = true;
        self.status.ready = self.status.motor_running && self.status.disk_present;
        Ok(self.status)
    }

    fn set_motor(&mut self, enabled: bool, quick: bool) -> Result<()> {
        if enabled {
            let values = if quick {
                [1_000_u16, 5_000, 150, 5, 10_000]
            } else {
                [1_000_u16, 5_000, 750, 15, 20_000]
            };
            let mut payload = Vec::with_capacity(10);
            for value in values {
                payload.extend_from_slice(&value.to_be_bytes());
            }
            self.expect_ok(Command::SetParams, &payload)?;
        }
        let command = match (self.drive_a, enabled) {
            (true, true) => Command::MotorAOn,
            (false, true) => Command::MotorBOn,
            (true, false) => Command::MotorAOff,
            (false, false) => Command::MotorBOff,
        };
        self.expect_ok(command, &[])?;
        self.status.motor_running = enabled;
        if enabled {
            self.select(true)?;
        } else {
            self.select(false)?;
        }
        Ok(())
    }

    fn seek(&mut self, cylinder: u8) -> Result<()> {
        if cylinder >= self.status.max_cylinders {
            return Err(Error::InvalidConfig(format!(
                "SuperCard Pro cylinder {cylinder} is out of range"
            )));
        }
        self.select(true)?;
        self.expect_ok(Command::StepTo, &[cylinder])?;
        self.status.cylinder = cylinder;
        if !self.status.motor_running {
            self.select(false)?;
        }
        Ok(())
    }

    fn select_side(&mut self, side: Side) -> Result<()> {
        self.select(true)?;
        self.expect_ok(Command::Side, &[side.number()])?;
        self.status.side = side;
        if !self.status.motor_running {
            self.select(false)?;
        }
        Ok(())
    }

    fn no_click_step(&mut self) -> Result<()> {
        if self.status.cylinder != 0 {
            return Err(Error::InvalidConfig(
                "no-click step is only valid at cylinder zero".into(),
            ));
        }
        self.select(true)?;
        self.expect_ok(Command::StepOut, &[])?;
        self.check_pins()
    }

    fn read_track(&mut self, mode: ReadMode, density: DensityMode) -> Result<RawCapture> {
        self.high_density =
            density == DensityMode::High || (density == DensityMode::Auto && self.high_density);
        self.read_stream(mode)
    }

    fn write_track(
        &mut self,
        words: &[u16],
        bit_len: usize,
        from_index: bool,
        density: DensityMode,
    ) -> Result<()> {
        self.high_density =
            density == DensityMode::High || (density == DensityMode::Auto && self.high_density);
        self.write_stream(words, bit_len, from_index)
    }
}

impl Drop for SuperCardPro {
    fn drop(&mut self) {
        let _ = self.set_motor(false, true);
    }
}

fn map_response(response: u8) -> Result<()> {
    match response {
        RESPONSE_OK => Ok(()),
        RESPONSE_WRITE_PROTECTED => Err(Error::WriteProtected),
        RESPONSE_NO_DISK | RESPONSE_NOT_READY => Err(Error::Timeout("SuperCard Pro disk ready")),
        RESPONSE_OVERRUN => Err(Error::Protocol("SuperCard Pro buffer overrun".into())),
        value => Err(Error::Protocol(format!(
            "SuperCard Pro returned response {value:#04x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_checksum_wraps_like_firmware() {
        let packet = SuperCardPro::packet(Command::StepTo, &[80]).unwrap();
        assert_eq!(
            packet,
            [
                0x89,
                1,
                80,
                0x4a_u8.wrapping_add(0x89).wrapping_add(1).wrapping_add(80)
            ]
        );
    }

    #[test]
    fn firmware_nibbles_compare_lexicographically() {
        assert!((2, 0) >= (1, 3));
        assert!((0, 9) < (1, 3));
    }
}
