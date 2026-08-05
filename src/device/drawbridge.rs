// SPDX-License-Identifier: LGPL-3.0-or-later

//! DrawBridge/Arduino Reader Writer protocol.

use std::thread;
use std::time::{Duration, Instant};

use crate::device::{Device, RawCapture, validate_capture};
use crate::flux::{bits_to_unaligned_revolution, pack_bits};
use crate::transport::{self, Transport, read_exact_until, write_all_until};
use crate::{
    BridgeConfig, DensityMode, DriveStatus, DriveType, Error, PortId, ReadMode, Result, Side,
};

const BAUD: u32 = 2_000_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const STREAM_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_STREAM_BYTES: usize = 1 << 20;
const FLAG_DENSITY_DETECT: u8 = 1 << 3;

/// Open DrawBridge device.
pub(super) struct DrawBridge {
    transport: Box<dyn Transport>,
    port: PortId,
    status: DriveStatus,
    firmware: (u8, u8, u8),
    flags: u8,
    full_control: bool,
    high_density: bool,
}

impl DrawBridge {
    pub(super) fn open(port: PortId, config: &BridgeConfig) -> Result<Box<dyn Device>> {
        let mut transport = transport::open(&port, BAUD, Duration::from_millis(250))?;
        transport.set_cts_flow_control(true)?;
        transport.purge()?;
        let (firmware, full_control) = match sync_version(transport.as_mut()) {
            Ok(version) => version,
            Err(_) => {
                transport.set_dtr(false)?;
                transport.set_rts(false)?;
                thread::sleep(Duration::from_millis(10));
                transport.set_dtr(true)?;
                transport.set_rts(true)?;
                thread::sleep(Duration::from_millis(150));
                transport.purge()?;
                sync_version(transport.as_mut())?
            }
        };
        if (firmware.0, firmware.1) < (1, 8) {
            return Err(Error::UnsupportedFirmware(format!(
                "DrawBridge firmware {}.{}; version 1.8 or later is required",
                firmware.0, firmware.1
            )));
        }

        let mut device = Self {
            transport,
            port,
            status: DriveStatus {
                max_cylinders: 84,
                ..DriveStatus::default()
            },
            firmware,
            flags: 0,
            full_control,
            high_density: config.density == DensityMode::High,
        };
        device.transport.set_timeout(COMMAND_TIMEOUT)?;
        device.transport.purge()?;
        if (firmware.0, firmware.1) >= (1, 9) {
            device.expect_command(b'@')?;
            let mut features = [0_u8; 3];
            read_exact_until(
                device.transport.as_mut(),
                &mut features,
                Instant::now() + COMMAND_TIMEOUT,
                "DrawBridge feature flags",
            )?;
            device.flags = features[0];
            device.firmware.2 = features[2];
        }
        device.expect_command(b'.')?;
        device.status.cylinder = 0;
        device.status.working = true;
        device.status.drive_type = if device.high_density {
            DriveType::Hd35
        } else {
            DriveType::Dd35
        };
        let _ = device.check_disk();
        Ok(Box::new(device))
    }

    fn send(&mut self, bytes: &[u8], operation: &'static str) -> Result<()> {
        write_all_until(
            self.transport.as_mut(),
            bytes,
            Instant::now() + COMMAND_TIMEOUT,
            operation,
        )
    }

    fn read_byte(&mut self, operation: &'static str) -> Result<u8> {
        let mut response = [0_u8];
        read_exact_until(
            self.transport.as_mut(),
            &mut response,
            Instant::now() + COMMAND_TIMEOUT,
            operation,
        )?;
        Ok(response[0])
    }

    fn expect_command(&mut self, command: u8) -> Result<()> {
        self.send(&[command], "DrawBridge command")?;
        match self.read_byte("DrawBridge command response")? {
            b'1' => Ok(()),
            b'0' => Err(Error::Protocol(format!(
                "DrawBridge rejected command {:?}",
                char::from(command)
            ))),
            response => Err(Error::Protocol(format!(
                "DrawBridge returned unexpected response {response:#04x}"
            ))),
        }
    }

    fn check_disk(&mut self) -> Result<()> {
        if (self.firmware.0, self.firmware.1) < (1, 8) {
            return Ok(());
        }
        self.send(b"^", "DrawBridge disk status")?;
        let disk = self.read_byte("DrawBridge disk status")?;
        let write_protect = self.read_byte("DrawBridge write-protect status")?;
        match disk {
            b'1' => self.status.disk_present = true,
            b'#' => self.status.disk_present = false,
            value => {
                return Err(Error::Protocol(format!(
                    "DrawBridge returned invalid disk status {value:#04x}"
                )));
            }
        }
        match write_protect {
            b'1' => self.status.write_protected = true,
            b'#' | b'0' => self.status.write_protected = false,
            _ => {}
        }
        Ok(())
    }

    fn choose_density(&mut self, density: DensityMode) -> Result<()> {
        let high = match density {
            DensityMode::Double => false,
            DensityMode::High => true,
            DensityMode::Auto if self.flags & FLAG_DENSITY_DETECT != 0 => {
                self.expect_command(b'T')?;
                match self.read_byte("DrawBridge density response")? {
                    b'H' => true,
                    b'D' => false,
                    b'x' => {
                        self.status.disk_present = false;
                        false
                    }
                    response => {
                        return Err(Error::Protocol(format!(
                            "DrawBridge returned invalid density {response:#04x}"
                        )));
                    }
                }
            }
            DensityMode::Auto => false,
        };
        if high != self.high_density {
            self.expect_command(if high { b'H' } else { b'D' })?;
            self.high_density = high;
            self.status.drive_type = if high {
                DriveType::Hd35
            } else {
                DriveType::Dd35
            };
        }
        Ok(())
    }

    fn read_stream(&mut self, mode: ReadMode) -> Result<RawCapture> {
        self.expect_command(b'{')?;
        let deadline = Instant::now() + STREAM_TIMEOUT;
        let target_bits = if self.high_density { 240_000 } else { 120_000 };
        let mut bits = Vec::with_capacity(target_bits + 8_192);
        let mut first_index = false;
        let mut complete = false;
        let mut bytes_seen = 0_usize;

        while !complete {
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
                "DrawBridge track stream",
            )?;
            bytes_seen += 1;
            let byte = byte[0];
            if self.high_density {
                for shift in [6, 4, 2, 0] {
                    let code = (byte >> shift) & 3;
                    let at_index = code == 3;
                    if at_index && mode != ReadMode::Fast {
                        if first_index {
                            complete = true;
                            break;
                        }
                        first_index = true;
                        bits.clear();
                    }
                    append_sequence(&mut bits, if code == 3 { 1 } else { code + 1 });
                }
            } else {
                if byte & 0x80 != 0 && mode != ReadMode::Fast {
                    if first_index {
                        complete = true;
                        continue;
                    }
                    first_index = true;
                    bits.clear();
                }
                append_sequence(&mut bits, (byte >> 5) & 3);
                append_sequence(&mut bits, (byte >> 3) & 3);
            }
            if mode == ReadMode::Fast && bits.len() >= target_bits {
                bits.truncate(target_bits);
                complete = true;
            }
        }

        self.send(b"x", "DrawBridge stop stream")?;
        let mut window = [0_u8; 5];
        loop {
            let byte = self.read_byte_until(deadline, "DrawBridge stop acknowledgement")?;
            window.rotate_left(1);
            window[4] = byte;
            if window == *b"XYZx1" {
                break;
            }
        }
        self.transport.purge()?;
        if bits.len() < 64 {
            return Err(Error::Protocol(
                "DrawBridge stream did not contain a complete revolution".into(),
            ));
        }
        if mode == ReadMode::Fast {
            bits = bits_to_unaligned_revolution(&bits, self.high_density)?;
        }
        let bit_len = bits.len();
        let capture = RawCapture {
            words: pack_bits(&bits),
            bit_len,
            index_aligned: mode != ReadMode::Fast && first_index,
        };
        validate_capture(&capture)?;
        self.status.disk_present = true;
        self.status.ready = true;
        Ok(capture)
    }

    fn read_byte_until(&mut self, deadline: Instant, operation: &'static str) -> Result<u8> {
        let mut byte = [0_u8];
        read_exact_until(self.transport.as_mut(), &mut byte, deadline, operation)?;
        Ok(byte[0])
    }

    fn write_stream(&mut self, words: &[u16], bit_len: usize, from_index: bool) -> Result<()> {
        let encoded = if self.high_density {
            encode_hd(words, bit_len)?
        } else {
            encode_dd(words, bit_len, self.status.cylinder >= 40)?
        };
        self.expect_command(if self.high_density { b'>' } else { b'}' })?;
        match self.read_byte("DrawBridge write permission")? {
            b'N' => {
                self.status.write_protected = true;
                return Err(Error::WriteProtected);
            }
            b'Y' => {}
            response => {
                return Err(Error::Protocol(format!(
                    "DrawBridge returned invalid write permission {response:#04x}"
                )));
            }
        }
        if !self.high_density {
            let length = u16::try_from(encoded.len()).map_err(|_| Error::TrackTooLarge {
                bits: bit_len,
                limit: crate::flux::MAX_TRACK_BITS,
            })?;
            self.send(&length.to_be_bytes(), "DrawBridge write length")?;
        }
        self.send(&[u8::from(from_index)], "DrawBridge write alignment")?;
        if self.read_byte("DrawBridge write ready")? != b'!' {
            return Err(Error::Protocol(
                "DrawBridge did not become ready for write data".into(),
            ));
        }
        write_all_until(
            self.transport.as_mut(),
            &encoded,
            Instant::now() + STREAM_TIMEOUT,
            "DrawBridge track write",
        )?;
        match self.read_byte_until(
            Instant::now() + STREAM_TIMEOUT,
            "DrawBridge write completion",
        )? {
            b'1' => Ok(()),
            b'X' => Err(Error::Timeout("DrawBridge track write")),
            b'Y' => Err(Error::Protocol("DrawBridge write framing error".into())),
            b'Z' => Err(Error::Protocol("DrawBridge serial write overrun".into())),
            response => Err(Error::Protocol(format!(
                "DrawBridge returned invalid write result {response:#04x}"
            ))),
        }
    }
}

impl Device for DrawBridge {
    fn selected_port(&self) -> &PortId {
        &self.port
    }

    fn status(&mut self) -> Result<DriveStatus> {
        self.check_disk()?;
        self.status.working = true;
        self.status.ready = self.status.motor_running && self.status.disk_present;
        Ok(self.status)
    }

    fn set_motor(&mut self, enabled: bool, quick: bool) -> Result<()> {
        self.expect_command(if enabled {
            if quick { b'*' } else { b'+' }
        } else {
            b'-'
        })?;
        self.status.motor_running = enabled;
        self.status.ready = enabled && self.status.disk_present;
        Ok(())
    }

    fn seek(&mut self, cylinder: u8) -> Result<()> {
        if cylinder >= self.status.max_cylinders {
            return Err(Error::InvalidConfig(format!(
                "DrawBridge cylinder {cylinder} is out of range"
            )));
        }
        let command = format!("={cylinder:02}{}", char::from(7));
        self.send(command.as_bytes(), "DrawBridge seek")?;
        match self.read_byte("DrawBridge seek response")? {
            b'2' => {}
            b'1' => {
                let disk = self.read_byte("DrawBridge seek disk status")?;
                let protected = self.read_byte("DrawBridge seek write-protect status")?;
                if disk != b'x' {
                    self.status.disk_present = disk == b'1';
                }
                self.status.write_protected = protected == b'1';
            }
            b'0' => return Err(Error::Protocol("DrawBridge seek failed".into())),
            value => {
                return Err(Error::Protocol(format!(
                    "DrawBridge returned invalid seek status {value:#04x}"
                )));
            }
        }
        self.status.cylinder = cylinder;
        Ok(())
    }

    fn select_side(&mut self, side: Side) -> Result<()> {
        self.expect_command(if side == Side::Lower { b'[' } else { b']' })?;
        self.status.side = side;
        Ok(())
    }

    fn no_click_step(&mut self) -> Result<()> {
        if !self.full_control {
            return Ok(());
        }
        self.expect_command(b'O')?;
        let disk = self.read_byte("DrawBridge no-click disk status")?;
        let protected = self.read_byte("DrawBridge no-click write-protect status")?;
        if disk != b'x' {
            self.status.disk_present = disk == b'1';
        }
        self.status.write_protected = protected == b'1';
        Ok(())
    }

    fn read_track(
        &mut self,
        mode: ReadMode,
        density: DensityMode,
        _progress: &mut dyn FnMut(Vec<u16>, usize),
    ) -> Result<RawCapture> {
        self.choose_density(density)?;
        self.read_stream(mode)
    }

    fn write_track(
        &mut self,
        words: &[u16],
        bit_len: usize,
        from_index: bool,
        density: DensityMode,
    ) -> Result<()> {
        self.choose_density(density)?;
        self.write_stream(words, bit_len, from_index)
    }
}

impl Drop for DrawBridge {
    fn drop(&mut self) {
        let _ = self.expect_command(b'-');
    }
}

fn sync_version(transport: &mut dyn Transport) -> Result<((u8, u8, u8), bool)> {
    write_all_until(
        transport,
        b"xR?",
        Instant::now() + Duration::from_secs(1),
        "DrawBridge version request",
    )?;
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut window = [0_u8; 5];
    loop {
        let mut byte = [0_u8];
        read_exact_until(
            transport,
            &mut byte,
            deadline,
            "DrawBridge version response",
        )?;
        window.rotate_left(1);
        window[4] = byte[0];
        if window[0] == b'1'
            && window[1] == b'V'
            && window[2].is_ascii_digit()
            && matches!(window[3], b'.' | b',')
            && window[4].is_ascii_digit()
        {
            transport.purge()?;
            return Ok(((window[2] - b'0', window[4] - b'0', 0), window[3] == b','));
        }
    }
}

fn append_sequence(bits: &mut Vec<bool>, code: u8) {
    match code {
        0 => bits.extend([false, false, false]),
        1 => bits.extend([false, true]),
        2 => bits.extend([false, false, true]),
        _ => bits.extend([false, false, false, true]),
    }
}

fn bit_at(words: &[u16], bit_len: usize, position: usize) -> bool {
    if position >= bit_len {
        // Safe alternating run-out, matching the original port.
        return (position - bit_len).is_multiple_of(2);
    }
    words[position / 16] & (1 << (15 - position % 16)) != 0
}

fn transition_counts(words: &[u16], bit_len: usize, limit: u8) -> Result<Vec<u8>> {
    if bit_len == 0 || bit_len > words.len() * 16 {
        return Err(Error::InvalidConfig(
            "DrawBridge write bit length does not fit its words".into(),
        ));
    }
    let mut counts = Vec::with_capacity(bit_len / 2);
    let mut count = 0_u8;
    let mut position = 0_usize;
    while position < bit_len + 8 {
        count = count.saturating_add(1);
        if bit_at(words, bit_len, position) {
            counts.push(count.clamp(2, limit));
            count = 0;
        }
        position += 1;
    }
    Ok(counts)
}

fn encode_dd(words: &[u16], bit_len: usize, precomp: bool) -> Result<Vec<u8>> {
    let counts = transition_counts(words, bit_len, 5)?;
    let mut output = Vec::with_capacity(counts.len().div_ceil(2));
    let mut previous = 2_u8;
    for chunk in counts.chunks(2) {
        let mut byte = 0_u8;
        for (index, &count) in chunk.iter().enumerate() {
            let adjustment = if precomp {
                if previous == 2 && count >= 4 {
                    0x04
                } else if previous >= 4 && count == 2 {
                    0x08
                } else {
                    0
                }
            } else {
                0
            };
            byte |= ((previous - 2) | adjustment) << (index * 4);
            previous = count;
        }
        output.push(byte);
    }
    Ok(output)
}

fn encode_hd(words: &[u16], bit_len: usize) -> Result<Vec<u8>> {
    let counts = transition_counts(words, bit_len, 4)?;
    let mut output = Vec::with_capacity(counts.len().div_ceil(4) + 1);
    for chunk in counts.chunks(4) {
        let mut byte = 0_u8;
        for (index, &count) in chunk.iter().enumerate() {
            let shift = match index {
                0 => 4,
                1 => 2,
                2 => 0,
                _ => 6,
            };
            byte |= (count - 1) << shift;
        }
        output.push(byte);
    }
    output.push(0);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_precision_sequences_expand() {
        let mut bits = Vec::new();
        for code in 0..4 {
            append_sequence(&mut bits, code);
        }
        assert_eq!(bits.len(), 3 + 2 + 3 + 4);
    }

    #[test]
    fn write_encoders_are_bounded_and_padded() {
        let words = vec![0xaaaa; 6_250];
        let dd = encode_dd(&words, 100_000, true).unwrap();
        let hd = encode_hd(&words, 100_000).unwrap();
        assert!(!dd.is_empty());
        assert_eq!(hd.last(), Some(&0));
    }
}
