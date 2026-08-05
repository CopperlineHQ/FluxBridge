// SPDX-License-Identifier: MPL-2.0 AND LGPL-3.0-or-later

//! Greaseweazle serial protocol.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::device::{Device, RawCapture, drive_selection, operation_deadline, validate_capture};
use crate::flux::{FluxEvent, flux_to_revolution, flux_to_unaligned_revolution, mfm_to_flux};
use crate::transport::{self, Transport, read_exact_until, write_all_until};
use crate::{
    BridgeConfig, DensityMode, DriveStatus, DriveType, Error, PortId, ReadMode, Result, Side,
};

const BAUD: u32 = 9_600;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const STREAM_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_STREAM_BYTES: usize = 1 << 20;

#[repr(u8)]
#[derive(Clone, Copy)]
enum Command {
    GetInfo = 0,
    Seek = 2,
    Head = 3,
    SetParams = 4,
    GetParams = 5,
    Motor = 6,
    ReadFlux = 7,
    WriteFlux = 8,
    GetFluxStatus = 9,
    Select = 12,
    Deselect = 13,
    SetBusType = 14,
    Reset = 16,
    GetPin = 20,
    NoClickStep = 22,
}

impl Command {
    const fn name(self) -> &'static str {
        match self {
            Self::GetInfo => "GetInfo",
            Self::Seek => "Seek",
            Self::Head => "Head",
            Self::SetParams => "SetParams",
            Self::GetParams => "GetParams",
            Self::Motor => "Motor",
            Self::ReadFlux => "ReadFlux",
            Self::WriteFlux => "WriteFlux",
            Self::GetFluxStatus => "GetFluxStatus",
            Self::Select => "Select",
            Self::Deselect => "Deselect",
            Self::SetBusType => "SetBusType",
            Self::Reset => "Reset",
            Self::GetPin => "GetPin",
            Self::NoClickStep => "NoClickStep",
        }
    }

    const fn acknowledgement_operation(self) -> &'static str {
        match self {
            Self::GetInfo => "Greaseweazle GetInfo acknowledgement",
            Self::Seek => "Greaseweazle Seek acknowledgement",
            Self::Head => "Greaseweazle Head acknowledgement",
            Self::SetParams => "Greaseweazle SetParams acknowledgement",
            Self::GetParams => "Greaseweazle GetParams acknowledgement",
            Self::Motor => "Greaseweazle Motor acknowledgement",
            Self::ReadFlux => "Greaseweazle ReadFlux acknowledgement",
            Self::WriteFlux => "Greaseweazle WriteFlux acknowledgement",
            Self::GetFluxStatus => "Greaseweazle GetFluxStatus acknowledgement",
            Self::Select => "Greaseweazle Select acknowledgement",
            Self::Deselect => "Greaseweazle Deselect acknowledgement",
            Self::SetBusType => "Greaseweazle SetBusType acknowledgement",
            Self::Reset => "Greaseweazle Reset acknowledgement",
            Self::GetPin => "Greaseweazle GetPin acknowledgement",
            Self::NoClickStep => "Greaseweazle NoClickStep acknowledgement",
        }
    }
}

const ACK_OK: u8 = 0;
const ACK_BAD_COMMAND: u8 = 1;
const ACK_NO_INDEX: u8 = 2;
const ACK_FLUX_OVERFLOW: u8 = 4;
const ACK_FLUX_UNDERFLOW: u8 = 5;
const ACK_WRITE_PROTECTED: u8 = 6;

/// Open Greaseweazle device.
pub(super) struct Greaseweazle {
    transport: Box<dyn Transport>,
    port: PortId,
    status: DriveStatus,
    sample_frequency: u32,
    delays: [u16; 5],
    bus: u8,
    drive: u8,
    selected: bool,
    high_density: bool,
}

impl Greaseweazle {
    pub(super) fn open(port: PortId, config: &BridgeConfig) -> Result<Box<dyn Device>> {
        let transport = transport::open(&port, BAUD, COMMAND_TIMEOUT)?;
        let (bus, drive) = drive_selection(config.drive);
        let mut device = Self {
            transport,
            port,
            status: DriveStatus {
                max_cylinders: 82,
                ..DriveStatus::default()
            },
            sample_frequency: 0,
            delays: [0; 5],
            bus,
            drive,
            selected: false,
            high_density: config.density == DensityMode::High,
        };
        device.prepare_transport()?;

        let version = device.get_firmware().or_else(|_| {
            device.transport.purge()?;
            device.get_firmware()
        })?;
        let major = version[0];
        let minor = version[1];
        if (major, minor) < (0, 27) {
            return Err(Error::UnsupportedFirmware(format!(
                "Greaseweazle {major}.{minor}; version 0.27 or later is required"
            )));
        }
        if version[2] == 0 {
            return Err(Error::UnsupportedFirmware(
                "Greaseweazle is running update firmware".into(),
            ));
        }
        device.sample_frequency =
            u32::from_le_bytes(version[4..8].try_into().expect("fixed firmware field"));
        if device.sample_frequency == 0 {
            return Err(Error::Protocol(
                "Greaseweazle reported a zero sample frequency".into(),
            ));
        }

        device.command(Command::Reset, &[], 0)?;
        let delay_bytes = device.command(Command::GetParams, &[0, 10], 10)?;
        for (index, chunk) in delay_bytes.chunks_exact(2).enumerate() {
            device.delays[index] = u16::from_le_bytes([chunk[0], chunk[1]]);
        }
        device.expect_ok(Command::SetBusType, &[bus])?;
        device.check_pins()?;
        device.status.working = true;
        device.status.drive_type = DriveType::Hd35;
        Ok(Box::new(device))
    }

    fn get_firmware(&mut self) -> Result<Vec<u8>> {
        self.command(Command::GetInfo, &[0], 32)
    }

    fn prepare_transport(&mut self) -> Result<()> {
        self.transport.set_dtr(true)?;
        self.transport.set_rts(true)?;
        self.transport.purge()
    }

    fn command(&mut self, command: Command, parameters: &[u8], extra: u8) -> Result<Vec<u8>> {
        let packet = command_packet(command, parameters)?;
        let deadline = operation_deadline();
        write_all_until(
            self.transport.as_mut(),
            &packet,
            deadline,
            "Greaseweazle command",
        )?;
        let mut response = [0_u8; 2];
        read_exact_until(
            self.transport.as_mut(),
            &mut response,
            deadline,
            command.acknowledgement_operation(),
        )?;
        if response[0] != command as u8 {
            return Err(Error::Protocol(format!(
                "Greaseweazle replied to command {:#04x} while {:#04x} was pending",
                response[0], command as u8
            )));
        }
        map_ack(command, response[1])?;
        let mut output = vec![0; usize::from(extra)];
        if !output.is_empty() {
            read_exact_until(
                self.transport.as_mut(),
                &mut output,
                deadline,
                "Greaseweazle command payload",
            )?;
        }
        Ok(output)
    }

    fn expect_ok(&mut self, command: Command, parameters: &[u8]) -> Result<()> {
        self.command(command, parameters, 0).map(drop)
    }

    fn select(&mut self, selected: bool) -> Result<()> {
        if selected == self.selected {
            return Ok(());
        }
        if selected {
            self.expect_ok(Command::Select, &[self.drive])?;
        } else {
            self.expect_ok(Command::Deselect, &[])?;
        }
        self.selected = selected;
        Ok(())
    }

    fn check_pins(&mut self) -> Result<()> {
        let deselect = !self.status.motor_running;
        self.select(true)?;
        let write_protect = self.command(Command::GetPin, &[28], 1)?;
        self.status.write_protected = write_protect[0] == 0;
        if self.bus == 1 {
            let disk_change = self.command(Command::GetPin, &[34], 1)?;
            self.status.disk_present = disk_change[0] == 1;
        } else {
            // Shugart pin 2 cannot be sampled by Greaseweazle. Treat media as
            // present so a read can determine whether flux and index pulses
            // actually exist; disk-change notification is necessarily
            // simulated for this bus type.
            self.status.disk_present = true;
        }
        if deselect {
            self.select(false)?;
        }
        Ok(())
    }

    fn update_delays(&mut self) -> Result<()> {
        let mut parameters = Vec::with_capacity(11);
        parameters.push(0);
        for delay in self.delays {
            parameters.extend_from_slice(&delay.to_le_bytes());
        }
        self.expect_ok(Command::SetParams, &parameters)
    }

    fn read_stream(&mut self, mode: ReadMode) -> Result<RawCapture> {
        self.select(true)?;
        let (ticks, max_index, linger) = if mode == ReadMode::Fast {
            (
                u32::try_from(u64::from(self.sample_frequency) * 260 / 1_000)
                    .expect("260ms tick count fits u32"),
                0_u16,
                0_u32,
            )
        } else {
            (
                0,
                2,
                u32::try_from(u64::from(self.sample_frequency) * 20 / 1_000)
                    .expect("20ms tick count fits u32"),
            )
        };
        let mut header = Vec::with_capacity(10);
        header.extend_from_slice(&ticks.to_le_bytes());
        header.extend_from_slice(&max_index.to_le_bytes());
        header.extend_from_slice(&linger.to_le_bytes());
        self.expect_ok(Command::ReadFlux, &header)?;

        let deadline = Instant::now() + STREAM_TIMEOUT;
        let mut stream = Vec::with_capacity(128 * 1024);
        loop {
            if stream.len() >= MAX_STREAM_BYTES {
                return Err(Error::TrackTooLarge {
                    bits: stream.len() * 8,
                    limit: MAX_STREAM_BYTES * 8,
                });
            }
            let mut byte = [0_u8];
            read_exact_until(
                self.transport.as_mut(),
                &mut byte,
                deadline,
                "Greaseweazle flux stream",
            )?;
            if byte[0] == 0 {
                break;
            }
            stream.push(byte[0]);
        }
        let status = self.raw_command_ack(Command::GetFluxStatus, &[])?;
        match status {
            ACK_OK => {
                self.status.disk_present = true;
                self.status.ready = true;
            }
            ACK_NO_INDEX => {
                self.status.disk_present = false;
                return Err(Error::Timeout("Greaseweazle index pulse"));
            }
            ACK_FLUX_OVERFLOW => {
                return Err(Error::Protocol(
                    "Greaseweazle flux receive buffer overflowed".into(),
                ));
            }
            other => map_ack(Command::GetFluxStatus, other)?,
        }
        if !self.status.motor_running {
            self.select(false)?;
        }

        let events = decode_stream(&stream, self.sample_frequency, self.high_density)?;
        let index_aligned = mode != ReadMode::Fast;
        let (words, bit_len, extracted_index) = if index_aligned {
            flux_to_revolution(&events)?
        } else {
            let (words, bits) = flux_to_unaligned_revolution(&events, self.high_density)?;
            (words, bits, false)
        };
        let capture = RawCapture {
            words,
            bit_len,
            index_aligned: index_aligned && extracted_index,
        };
        validate_capture(&capture)?;
        Ok(capture)
    }

    fn raw_command_ack(&mut self, command: Command, parameters: &[u8]) -> Result<u8> {
        let packet = command_packet(command, parameters)?;
        let deadline = operation_deadline();
        write_all_until(
            self.transport.as_mut(),
            &packet,
            deadline,
            "Greaseweazle command",
        )?;
        let mut response = [0_u8; 2];
        read_exact_until(
            self.transport.as_mut(),
            &mut response,
            deadline,
            command.acknowledgement_operation(),
        )?;
        if response[0] != command as u8 {
            return Err(Error::Protocol(
                "Greaseweazle command acknowledgement was out of sequence".into(),
            ));
        }
        Ok(response[1])
    }

    fn write_stream(&mut self, words: &[u16], bit_len: usize, from_index: bool) -> Result<()> {
        let cell_ns = if self.high_density { 1_000 } else { 2_000 };
        let timings = mfm_to_flux(words, bit_len, cell_ns)?;
        let mut stream = Vec::with_capacity(timings.len() * 2 + 1);
        for nanoseconds in timings {
            let ticks = ((u64::from(nanoseconds) * u64::from(self.sample_frequency) + 500_000_000)
                / 1_000_000_000)
                .max(1);
            encode_ticks(
                u32::try_from(ticks)
                    .map_err(|_| Error::Protocol("Greaseweazle flux delay overflow".into()))?,
                &mut stream,
            );
        }
        stream.push(0);

        self.select(true)?;
        let ack = self.raw_command_ack(Command::WriteFlux, &[u8::from(from_index), 0])?;
        match ack {
            ACK_OK => {}
            ACK_WRITE_PROTECTED => return Err(Error::WriteProtected),
            other => map_ack(Command::WriteFlux, other)?,
        }
        let deadline = Instant::now() + STREAM_TIMEOUT;
        write_all_until(
            self.transport.as_mut(),
            &stream,
            deadline,
            "Greaseweazle flux write",
        )?;
        let mut sync = [0_u8];
        read_exact_until(
            self.transport.as_mut(),
            &mut sync,
            deadline,
            "Greaseweazle write completion",
        )?;
        match self.raw_command_ack(Command::GetFluxStatus, &[])? {
            ACK_OK => Ok(()),
            ACK_WRITE_PROTECTED => Err(Error::WriteProtected),
            ACK_FLUX_UNDERFLOW => Err(Error::Protocol(
                "Greaseweazle flux transmit buffer underflowed".into(),
            )),
            other => {
                map_ack(Command::GetFluxStatus, other)?;
                Ok(())
            }
        }
    }
}

fn command_packet(command: Command, parameters: &[u8]) -> Result<Vec<u8>> {
    let mut packet = Vec::with_capacity(parameters.len() + 2);
    packet.push(command as u8);
    packet.push(
        u8::try_from(parameters.len() + 2)
            .map_err(|_| Error::InvalidConfig("Greaseweazle command too long".into()))?,
    );
    packet.extend_from_slice(parameters);
    Ok(packet)
}

impl Device for Greaseweazle {
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
        let desired_delay = if quick { 10 } else { 750 };
        if self.delays[3] != desired_delay {
            self.delays[3] = desired_delay;
            self.update_delays()?;
        }
        self.expect_ok(Command::Motor, &[self.drive, u8::from(enabled)])?;
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
                "Greaseweazle cylinder {cylinder} is out of range"
            )));
        }
        self.select(true)?;
        self.expect_ok(Command::Seek, &[cylinder])?;
        self.status.cylinder = cylinder;
        if !self.status.motor_running {
            self.select(false)?;
        }
        Ok(())
    }

    fn select_side(&mut self, side: Side) -> Result<()> {
        self.expect_ok(Command::Head, &[side.number()])?;
        self.status.side = side;
        Ok(())
    }

    fn no_click_step(&mut self) -> Result<()> {
        self.select(true)?;
        self.expect_ok(Command::NoClickStep, &[])?;
        self.check_pins()
    }

    fn read_track(&mut self, mode: ReadMode, density: DensityMode) -> Result<RawCapture> {
        self.high_density = match density {
            DensityMode::Double => false,
            DensityMode::High => true,
            DensityMode::Auto => self.high_density,
        };
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

impl Drop for Greaseweazle {
    fn drop(&mut self) {
        let _ = self.set_motor(false, true);
    }
}

fn map_ack(command: Command, ack: u8) -> Result<()> {
    match ack {
        ACK_OK => Ok(()),
        ACK_BAD_COMMAND => Err(Error::Protocol(format!(
            "Greaseweazle rejected unsupported or malformed {} command",
            command.name()
        ))),
        ACK_NO_INDEX => Err(Error::Timeout("Greaseweazle index pulse")),
        ACK_WRITE_PROTECTED => Err(Error::WriteProtected),
        ACK_FLUX_OVERFLOW => Err(Error::Protocol("Greaseweazle flux overflow".into())),
        ACK_FLUX_UNDERFLOW => Err(Error::Protocol("Greaseweazle flux underflow".into())),
        value => Err(Error::Protocol(format!(
            "Greaseweazle returned acknowledgement {value}"
        ))),
    }
}

fn read_28bit(queue: &mut VecDeque<u8>) -> Result<u32> {
    let mut value = 0_u32;
    for shift in [1, 8, 15, 22] {
        let byte = queue
            .pop_front()
            .ok_or_else(|| Error::Protocol("truncated Greaseweazle 28-bit value".into()))?;
        value |= u32::from(byte >> 1) << shift;
    }
    Ok(value >> 1)
}

fn decode_stream(bytes: &[u8], frequency: u32, high_density: bool) -> Result<Vec<FluxEvent>> {
    let mut queue = VecDeque::from(bytes.to_vec());
    let mut ticks = 0_u32;
    let mut index = false;
    let mut events = Vec::with_capacity(bytes.len());
    while let Some(byte) = queue.pop_front() {
        if byte == 255 {
            let opcode = queue
                .pop_front()
                .ok_or_else(|| Error::Protocol("truncated Greaseweazle opcode".into()))?;
            match opcode {
                1 => {
                    let _index_offset = read_28bit(&mut queue)?;
                    index = true;
                }
                2 => ticks = ticks.saturating_add(read_28bit(&mut queue)?),
                3 => {
                    let _period = queue.pop_front().ok_or_else(|| {
                        Error::Protocol("truncated Greaseweazle astable opcode".into())
                    })?;
                }
                value => {
                    return Err(Error::Protocol(format!(
                        "unknown Greaseweazle flux opcode {value}"
                    )));
                }
            }
            continue;
        }
        if byte < 250 {
            ticks = ticks.saturating_add(u32::from(byte));
        } else {
            let low = queue
                .pop_front()
                .ok_or_else(|| Error::Protocol("truncated Greaseweazle flux interval".into()))?;
            ticks = ticks.saturating_add(
                250 + (u32::from(byte) - 250) * 255 + u32::from(low).saturating_sub(1),
            );
        }
        let mut nanoseconds = u32::try_from(
            (u64::from(ticks) * 1_000_000_000 + u64::from(frequency) / 2) / u64::from(frequency),
        )
        .map_err(|_| Error::Protocol("Greaseweazle flux interval is too large".into()))?;
        if high_density {
            nanoseconds = nanoseconds.saturating_mul(2);
        }
        events.push(FluxEvent { nanoseconds, index });
        ticks = 0;
        index = false;
    }
    Ok(events)
}

fn encode_28bit(value: u32, output: &mut Vec<u8>) {
    output.push(1 | ((value << 1) & 0xff) as u8);
    output.push(1 | ((value >> 6) & 0xff) as u8);
    output.push(1 | ((value >> 13) & 0xff) as u8);
    output.push(1 | ((value >> 20) & 0xff) as u8);
}

fn encode_ticks(ticks: u32, output: &mut Vec<u8>) {
    if ticks < 250 {
        output.push(u8::try_from(ticks).expect("ticks less than 250 fit u8"));
        return;
    }
    let high = (ticks - 250) / 255;
    if high < 5 {
        output.push(u8::try_from(250 + high).expect("short extension fits u8"));
        output.push(u8::try_from(1 + (ticks - 250) % 255).expect("extension remainder fits u8"));
    } else {
        output.extend_from_slice(&[255, 2]);
        encode_28bit(ticks - 249, output);
        output.push(249);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_length_covers_only_request_bytes() {
        assert_eq!(
            command_packet(Command::GetInfo, &[0]).unwrap(),
            [Command::GetInfo as u8, 3, 0]
        );
        assert_eq!(
            command_packet(Command::GetParams, &[0]).unwrap(),
            [Command::GetParams as u8, 3, 0]
        );
    }

    #[test]
    fn flux_tick_forms_decode() {
        let bytes = [80, 250, 6, 255, 2, 21, 1, 1, 1, 40];
        let events = decode_stream(&bytes, 40_000_000, false).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].nanoseconds, 2_000);
        assert!(events.iter().all(|event| event.nanoseconds > 0));
    }

    #[test]
    fn tick_encoding_round_trips_common_values() {
        for ticks in [80, 160, 240, 250, 700, 2_000] {
            let mut bytes = Vec::new();
            encode_ticks(ticks, &mut bytes);
            let events = decode_stream(&bytes, 40_000_000, false).unwrap();
            assert_eq!(events.len(), 1);
            let decoded = events[0].nanoseconds / 25;
            assert_eq!(decoded, ticks);
        }
    }
}
