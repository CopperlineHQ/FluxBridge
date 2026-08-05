// SPDX-License-Identifier: LGPL-3.0-or-later

//! Safe Rust access to physical floppy drives through Greaseweazle,
//! DrawBridge, and SuperCard Pro interfaces.
//!
//! FluxBridge began as a Rust port of the runtime portions of Rob Smith's
//! [FloppyDriveBridge](https://github.com/RobSmithDev/FloppyDriveBridge) and
//! has since been substantially reworked around an emulator's real needs. It
//! exposes typed configuration, non-blocking track capture that can stream a
//! revolution while the platter is still turning it, and observable
//! asynchronous writes, all without a C or C++ ABI.

#![forbid(unsafe_code)]

mod controller;
mod device;
mod error;
pub mod flux;
mod transport;
mod types;

pub use controller::Bridge;
pub use error::{Error, ErrorKind, Result};
pub use types::{
    BridgeConfig, BridgeEvent, Capabilities, CaptureQuality, DensityMode, DriveSelect, DriveStatus,
    DriveType, DriverInfo, DriverKind, PortId, PortInfo, PortSelection, ReadMode, Side,
    TrackAddress, TrackCapture, WriteId, WriteRequest,
};

/// The FluxBridge crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns descriptions of the drivers compiled into this build.
pub fn drivers() -> &'static [DriverInfo] {
    types::DRIVERS
}

/// Enumerates serial and direct-FTDI interfaces visible to the process.
///
/// Enumeration does not open or probe devices. A listed port may therefore
/// be unrelated USB serial hardware.
pub fn ports() -> Result<Vec<PortInfo>> {
    transport::list_ports()
}
