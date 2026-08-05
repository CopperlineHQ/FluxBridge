// SPDX-License-Identifier: LGPL-3.0-or-later

//! Bounded serial and direct-FTDI transports.

#![cfg_attr(
    not(any(
        feature = "drawbridge",
        feature = "greaseweazle",
        feature = "supercard-pro"
    )),
    allow(dead_code)
)]

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::{Error, PortId, PortInfo, Result};

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(250);

pub(crate) trait Transport: Read + Write + Send {
    fn set_timeout(&mut self, timeout: Duration) -> Result<()>;
    fn set_cts_flow_control(&mut self, enabled: bool) -> Result<()>;
    fn set_dtr(&mut self, asserted: bool) -> Result<()>;
    fn set_rts(&mut self, asserted: bool) -> Result<()>;
    fn purge(&mut self) -> Result<()>;
}

pub(crate) fn read_exact_until(
    transport: &mut dyn Transport,
    mut output: &mut [u8],
    deadline: Instant,
    operation: &'static str,
) -> Result<()> {
    while !output.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Timeout(operation))?;
        transport.set_timeout(remaining.min(DEFAULT_TIMEOUT))?;
        match transport.read(output) {
            Ok(0) => return Err(Error::Disconnected),
            Ok(read) => output = &mut output[read..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn write_all_until(
    transport: &mut dyn Transport,
    mut input: &[u8],
    deadline: Instant,
    operation: &'static str,
) -> Result<()> {
    while !input.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Timeout(operation))?;
        transport.set_timeout(remaining.min(DEFAULT_TIMEOUT))?;
        match transport.write(input) {
            Ok(0) => return Err(Error::Disconnected),
            Ok(written) => input = &input[written..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    transport.flush()?;
    Ok(())
}

pub(crate) fn open(id: &PortId, baud: u32, timeout: Duration) -> Result<Box<dyn Transport>> {
    if id.is_direct_ftdi() {
        #[cfg(feature = "direct-ftdi")]
        return ftdi::open(id, baud, timeout);
        #[cfg(not(feature = "direct-ftdi"))]
        return Err(Error::InvalidConfig(
            "direct FTDI support is not compiled in".into(),
        ));
    }
    serial::open(id, baud, timeout)
}

pub(crate) fn list_ports() -> Result<Vec<PortInfo>> {
    let mut ports = serial::list()?;
    #[cfg(feature = "direct-ftdi")]
    ports.extend(ftdi::list()?);
    ports.sort_by(|left, right| left.id.cmp(&right.id));
    ports.dedup_by(|left, right| left.id == right.id);
    Ok(ports)
}

mod serial {
    use super::*;
    use serialport::{ClearBuffer, SerialPortType};

    struct SerialTransport(Box<dyn serialport::SerialPort>);

    impl Read for SerialTransport {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.0.read(output)
        }
    }

    impl Write for SerialTransport {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.0.write(input)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    impl Transport for SerialTransport {
        fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
            self.0.set_timeout(timeout).map_err(map_error)
        }

        fn set_cts_flow_control(&mut self, enabled: bool) -> Result<()> {
            self.0
                .set_flow_control(if enabled {
                    serialport::FlowControl::Hardware
                } else {
                    serialport::FlowControl::None
                })
                .map_err(map_error)
        }

        fn set_dtr(&mut self, asserted: bool) -> Result<()> {
            self.0
                .write_data_terminal_ready(asserted)
                .map_err(map_error)
        }

        fn set_rts(&mut self, asserted: bool) -> Result<()> {
            self.0.write_request_to_send(asserted).map_err(map_error)
        }

        fn purge(&mut self) -> Result<()> {
            self.0.clear(ClearBuffer::All).map_err(map_error)
        }
    }

    pub(super) fn open(id: &PortId, baud: u32, timeout: Duration) -> Result<Box<dyn Transport>> {
        // serialport opens exclusively by default on Unix; Windows creates
        // the COM handle without sharing. Its explicit `exclusive` builder
        // method is therefore unnecessary and is not available on Windows.
        let port = serialport::new(id.as_str(), baud)
            .timeout(timeout)
            .dtr_on_open(true)
            .open()
            .map_err(map_error)?;
        Ok(Box::new(SerialTransport(port)))
    }

    /// Whether a serial device node is one that can actually be opened.
    ///
    /// macOS exposes every serial device twice: `/dev/cu.*` is the call-out
    /// node, and `/dev/tty.*` the call-in node, which blocks on open until
    /// carrier detect is asserted. A USB CDC device never asserts it, so
    /// opening the `tty` node hangs. Listing only the `cu` node also stops one
    /// interface appearing as two.
    #[cfg(target_os = "macos")]
    fn is_openable(port_name: &str) -> bool {
        !port_name.starts_with("/dev/tty.")
    }

    #[cfg(not(target_os = "macos"))]
    fn is_openable(_port_name: &str) -> bool {
        true
    }

    pub(super) fn list() -> Result<Vec<PortInfo>> {
        serialport::available_ports()
            .map_err(map_error)
            .and_then(|ports| {
                ports
                    .into_iter()
                    .filter(|port| is_openable(&port.port_name))
                    .map(|port| {
                        let (vid, pid, serial_number, product) = match port.port_type {
                            SerialPortType::UsbPort(usb) => {
                                (Some(usb.vid), Some(usb.pid), usb.serial_number, usb.product)
                            }
                            _ => (None, None, None, None),
                        };
                        Ok(PortInfo {
                            id: PortId::new(&port.port_name)?,
                            display_name: product.as_ref().map_or_else(
                                || port.port_name.clone(),
                                |name| format!("{name} ({})", port.port_name),
                            ),
                            vid,
                            pid,
                            serial_number,
                            product,
                        })
                    })
                    .collect()
            })
    }

    fn map_error(error: serialport::Error) -> Error {
        use serialport::ErrorKind;
        let message = error.to_string();
        match error.kind() {
            ErrorKind::NoDevice => Error::PortNotFound(message),
            ErrorKind::Io(io::ErrorKind::PermissionDenied) => Error::PermissionDenied(message),
            ErrorKind::Io(io::ErrorKind::WouldBlock) => Error::PortInUse(message),
            ErrorKind::Io(kind) => Error::Io(io::Error::new(kind, message)),
            ErrorKind::InvalidInput | ErrorKind::Unknown => Error::Protocol(message),
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn two_mbaud_is_representable_without_raw_termios_constants() {
            let _builder = serialport::new("not-opened", 2_000_000);
        }
    }
}

#[cfg(feature = "direct-ftdi")]
mod ftdi {
    use super::*;
    use ftdi_nusb::{
        DataBits, DeviceFilter, FlowControl, FtdiDevice, Interface, Parity, StopBits, find_devices,
    };

    const FTDI_VID: u16 = 0x0403;
    const FTDI_PIDS: [u16; 4] = [0x6001, 0x6010, 0x6011, 0x6014];

    struct FtdiTransport(FtdiDevice);

    impl Read for FtdiTransport {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.0.read(output).map_err(io::Error::other)
        }
    }

    impl Write for FtdiTransport {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.0.write(input).map_err(io::Error::other)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush().map_err(io::Error::other)
        }
    }

    impl Transport for FtdiTransport {
        fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
            self.0.set_read_timeout(timeout);
            self.0.set_write_timeout(timeout);
            Ok(())
        }

        fn set_cts_flow_control(&mut self, enabled: bool) -> Result<()> {
            self.0
                .set_flow_control(if enabled {
                    FlowControl::RtsCts
                } else {
                    FlowControl::Disabled
                })
                .map_err(|error| Error::Protocol(error.to_string()))
        }

        fn set_dtr(&mut self, asserted: bool) -> Result<()> {
            self.0
                .set_dtr(asserted)
                .map_err(|error| Error::Protocol(error.to_string()))
        }

        fn set_rts(&mut self, asserted: bool) -> Result<()> {
            self.0
                .set_rts(asserted)
                .map_err(|error| Error::Protocol(error.to_string()))
        }

        fn purge(&mut self) -> Result<()> {
            self.0
                .flush_all()
                .map_err(|error| Error::Protocol(error.to_string()))
        }
    }

    pub(super) fn list() -> Result<Vec<PortInfo>> {
        let mut output = Vec::new();
        for pid in FTDI_PIDS {
            let devices =
                find_devices(FTDI_VID, pid).map_err(|error| Error::Protocol(error.to_string()))?;
            for (index, device) in devices.into_iter().enumerate() {
                let serial = device.serial_number().map(str::to_owned);
                let selector = serial.clone().unwrap_or_else(|| index.to_string());
                let id = PortId::new(format!("ftdi:{FTDI_VID:04x}:{pid:04x}:{selector}"))?;
                let product = device.product_string().map(str::to_owned);
                output.push(PortInfo {
                    display_name: match (&product, &serial) {
                        (Some(product), Some(serial)) => {
                            format!("{product} ({serial}, direct USB)")
                        }
                        (Some(product), None) => format!("{product} (direct USB #{index})"),
                        _ => format!("FTDI {pid:04x} (direct USB #{index})"),
                    },
                    id,
                    vid: Some(FTDI_VID),
                    pid: Some(pid),
                    serial_number: serial,
                    product,
                });
            }
        }
        Ok(output)
    }

    pub(super) fn open(id: &PortId, baud: u32, timeout: Duration) -> Result<Box<dyn Transport>> {
        let mut fields = id.as_str().splitn(4, ':');
        let prefix = fields.next();
        let vid = fields
            .next()
            .and_then(|value| u16::from_str_radix(value, 16).ok());
        let pid = fields
            .next()
            .and_then(|value| u16::from_str_radix(value, 16).ok());
        let selector = fields.next();
        let (Some("ftdi"), Some(vid), Some(pid), Some(selector)) = (prefix, vid, pid, selector)
        else {
            return Err(Error::InvalidConfig(format!(
                "malformed direct FTDI port identifier {}",
                id.as_str()
            )));
        };

        let mut filter = DeviceFilter::new(vid, pid);
        if let Ok(index) = selector.parse::<usize>() {
            filter = filter.index(index);
        } else {
            filter = filter.serial(selector);
        }
        let mut device = FtdiDevice::open_with_filter(&filter, Interface::Any)
            .map_err(|error| Error::PortNotFound(error.to_string()))?;
        device
            .set_baudrate(baud)
            .and_then(|()| device.set_line_property(DataBits::Eight, StopBits::One, Parity::None))
            .and_then(|()| device.set_flow_control(FlowControl::Disabled))
            .map_err(|error| Error::Protocol(error.to_string()))?;
        device.set_read_timeout(timeout);
        device.set_write_timeout(timeout);
        device
            .flush_all()
            .map_err(|error| Error::Protocol(error.to_string()))?;
        Ok(Box::new(FtdiTransport(device)))
    }
}
