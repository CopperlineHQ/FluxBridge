// SPDX-License-Identifier: LGPL-3.0-or-later

//! Public value types.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use bitflags::bitflags;

use crate::{Error, Result};

bitflags! {
    /// Optional facilities implemented by a bridge driver.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Capabilities: u32 {
        /// Background capture of tracks adjacent to the active track.
        /// Automatic serial-port discovery.
        const AUTO_DETECT_PORT = 1 << 1;
        /// IBM PC Drive A/Drive B cable selection.
        const PC_DRIVE_SELECT = 1 << 2;
        /// Shugart drive-select lines 0 through 3.
        const SHUGART_DRIVE_SELECT = 1 << 3;
        /// High-density media.
        const HIGH_DENSITY = 1 << 4;
        /// Direct access to an FTDI USB interface.
        const DIRECT_FTDI = 1 << 5;
    }
}

/// A supported hardware protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DriverKind {
    /// DrawBridge/Arduino floppy interfaces.
    DrawBridge,
    /// Greaseweazle interfaces.
    Greaseweazle,
    /// SuperCard Pro interfaces.
    SuperCardPro,
}

impl DriverKind {
    /// Returns the stable configuration token.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DrawBridge => "drawbridge",
            Self::Greaseweazle => "greaseweazle",
            Self::SuperCardPro => "supercardpro",
        }
    }
}

impl fmt::Display for DriverKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DriverKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let normalized: String = value
            .chars()
            .filter(|character| !matches!(character, ' ' | '-' | '_'))
            .flat_map(char::to_lowercase)
            .collect();
        match normalized.as_str() {
            "drawbridge" | "arduino" => Ok(Self::DrawBridge),
            "greaseweazle" => Ok(Self::Greaseweazle),
            "supercardpro" | "scp" => Ok(Self::SuperCardPro),
            _ => Err(Error::InvalidConfig(format!(
                "unknown driver kind {value:?}"
            ))),
        }
    }
}

/// Static description of a driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverInfo {
    /// Stable driver identifier.
    pub kind: DriverKind,
    /// User-facing name.
    pub name: &'static str,
    /// Hardware manufacturer or project.
    pub manufacturer: &'static str,
    /// Project information URL.
    pub url: &'static str,
    /// Optional facilities supported by the driver.
    pub capabilities: Capabilities,
}

/// Drivers compiled into this build.
pub(crate) static DRIVERS: &[DriverInfo] = &[
    #[cfg(feature = "drawbridge")]
    DriverInfo {
        kind: DriverKind::DrawBridge,
        name: "DrawBridge",
        manufacturer: "RobSmithDev",
        url: "https://amiga.robsmithdev.co.uk/",
        capabilities: Capabilities::AUTO_DETECT_PORT
            .union(Capabilities::HIGH_DENSITY)
            .union(Capabilities::DIRECT_FTDI),
    },
    #[cfg(feature = "greaseweazle")]
    DriverInfo {
        kind: DriverKind::Greaseweazle,
        name: "Greaseweazle",
        manufacturer: "Keir Fraser",
        url: "https://github.com/keirf/greaseweazle",
        capabilities: Capabilities::AUTO_DETECT_PORT
            .union(Capabilities::PC_DRIVE_SELECT)
            .union(Capabilities::SHUGART_DRIVE_SELECT)
            .union(Capabilities::HIGH_DENSITY),
    },
    #[cfg(feature = "supercard-pro")]
    DriverInfo {
        kind: DriverKind::SuperCardPro,
        name: "SuperCard Pro",
        manufacturer: "CBMSTUFF.COM",
        url: "https://www.cbmstuff.com/",
        capabilities: Capabilities::AUTO_DETECT_PORT
            .union(Capabilities::PC_DRIVE_SELECT)
            .union(Capabilities::HIGH_DENSITY),
    },
];

/// A stable serial or direct-FTDI port identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PortId(String);

impl PortId {
    /// Creates an identifier from a system port path or FluxBridge `ftdi:` token.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() || value.contains('\0') {
            return Err(Error::InvalidConfig(
                "a port identifier must be non-empty and contain no NUL".into(),
            ));
        }
        Ok(Self(value))
    }

    /// Returns the persistent identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns whether this selects a direct FTDI interface.
    pub fn is_direct_ftdi(&self) -> bool {
        self.0.starts_with("ftdi:")
    }
}

impl fmt::Display for PortId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PortId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

/// Information about an enumerated port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    /// Stable identifier used in [`PortSelection`].
    pub id: PortId,
    /// User-facing port label.
    pub display_name: String,
    /// USB vendor identifier, when reported by the operating system.
    pub vid: Option<u16>,
    /// USB product identifier, when reported by the operating system.
    pub pid: Option<u16>,
    /// USB serial number, when reported.
    pub serial_number: Option<String>,
    /// USB product name, when reported.
    pub product: Option<String>,
}

/// How a bridge port is selected.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PortSelection {
    /// Probe suitable visible ports with bounded timeouts.
    #[default]
    Auto,
    /// Open one exact port and do not fall back to another.
    Exact(PortId),
}

/// Capture strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadMode {
    /// Capture without waiting for the index pulse.
    Fast,
    /// Capture an index-aligned revolution.
    #[default]
    Compatible,
    /// Wait briefly for a capture when none is already ready.
    Stalling,
}

/// Media density selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DensityMode {
    /// Detect density from the interface and disk.
    #[default]
    Auto,
    /// Force double density.
    Double,
    /// Force high density.
    High,
}

/// Physical drive-select line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriveSelect {
    /// IBM PC Drive A.
    #[default]
    PcA,
    /// IBM PC Drive B.
    PcB,
    /// Shugart select 0.
    Shugart0,
    /// Shugart select 1.
    Shugart1,
    /// Shugart select 2.
    Shugart2,
    /// Shugart select 3.
    Shugart3,
}

/// A disk surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Side {
    /// Lower surface, conventionally side 0.
    #[default]
    Lower,
    /// Upper surface, conventionally side 1.
    Upper,
}

impl Side {
    /// Returns the conventional numeric side.
    pub const fn number(self) -> u8 {
        match self {
            Self::Lower => 0,
            Self::Upper => 1,
        }
    }
}

impl From<bool> for Side {
    fn from(value: bool) -> Self {
        if value { Self::Upper } else { Self::Lower }
    }
}

impl From<Side> for bool {
    fn from(value: Side) -> Self {
        value == Side::Upper
    }
}

/// A physical cylinder and side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TrackAddress {
    /// Zero-based cylinder.
    pub cylinder: u8,
    /// Disk surface.
    pub side: Side,
}

/// Physical drive mechanism reported by the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriveType {
    /// 3.5-inch double-density drive.
    #[default]
    Dd35,
    /// 3.5-inch high-density drive, also able to use DD media.
    Hd35,
    /// 5.25-inch single-density drive.
    Sd525,
}

/// Configuration used to open one bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// Hardware protocol.
    pub driver: DriverKind,
    /// Capture strategy.
    pub mode: ReadMode,
    /// Density selection.
    pub density: DensityMode,
    /// Physical drive-select line.
    pub drive: DriveSelect,
    /// Port selection.
    pub port: PortSelection,
    /// Maximum duration for a stalling read.
    pub stall_timeout: Duration,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            driver: DriverKind::DrawBridge,
            mode: ReadMode::Compatible,
            density: DensityMode::Auto,
            drive: DriveSelect::PcA,
            port: PortSelection::Auto,
            stall_timeout: Duration::from_millis(450),
        }
    }
}

impl BridgeConfig {
    /// Validates this configuration against the selected compiled driver.
    pub fn validate(&self) -> Result<()> {
        let driver = DRIVERS
            .iter()
            .find(|driver| driver.kind == self.driver)
            .ok_or_else(|| {
                Error::InvalidConfig(format!("{} support is not compiled in", self.driver))
            })?;
        let needed = match self.drive {
            DriveSelect::PcA => None,
            DriveSelect::PcB => Some(Capabilities::PC_DRIVE_SELECT),
            DriveSelect::Shugart0
            | DriveSelect::Shugart1
            | DriveSelect::Shugart2
            | DriveSelect::Shugart3 => Some(Capabilities::SHUGART_DRIVE_SELECT),
        };
        if needed.is_some_and(|needed| !driver.capabilities.contains(needed)) {
            return Err(Error::InvalidConfig(format!(
                "{} does not support {:?}",
                self.driver, self.drive
            )));
        }
        if self.density == DensityMode::High
            && !driver.capabilities.contains(Capabilities::HIGH_DENSITY)
        {
            return Err(Error::InvalidConfig(format!(
                "{} does not support high-density media",
                self.driver
            )));
        }
        if self.stall_timeout.is_zero() {
            return Err(Error::InvalidConfig(
                "stall timeout must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// Whether a capture can safely be replayed as a ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureQuality {
    /// The interface captured from one index pulse to the next.
    IndexAligned,
    /// An index-less capture decoded as a complete valid AmigaDOS track.
    VerifiedAmigaDos {
        /// Number of distinct valid sectors.
        sectors: u8,
    },
    /// The join could not be proven seamless; consume this capture once.
    Unverified,
}

impl CaptureQuality {
    /// Returns whether the capture can be retained and replayed.
    pub const fn reusable(self) -> bool {
        !matches!(self, Self::Unverified)
    }
}

/// One complete captured revolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackCapture {
    words: Vec<u16>,
    bit_len: usize,
    quality: CaptureQuality,
    generation: u64,
}

impl TrackCapture {
    pub(crate) fn new(
        words: Vec<u16>,
        bit_len: usize,
        quality: CaptureQuality,
        generation: u64,
    ) -> Self {
        Self {
            words,
            bit_len,
            quality,
            generation,
        }
    }

    /// Packed, most-significant-bit-first MFM words.
    pub fn words(&self) -> &[u16] {
        &self.words
    }

    /// Consumes the capture and returns its packed MFM words.
    pub fn into_words(self) -> Vec<u16> {
        self.words
    }

    /// Exact number of meaningful bits before the revolution wraps.
    pub const fn bit_len(&self) -> usize {
        self.bit_len
    }

    /// Validation result for the revolution boundary.
    pub const fn quality(&self) -> CaptureQuality {
        self.quality
    }

    /// Monotonic capture number assigned by this bridge.
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// Snapshot of the most recently observed drive state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveStatus {
    /// Worker and transport are healthy.
    pub working: bool,
    /// Drive is ready for track operations.
    pub ready: bool,
    /// A disk is present.
    pub disk_present: bool,
    /// The disk's write-protect tab is closed.
    pub write_protected: bool,
    /// The motor is running.
    pub motor_running: bool,
    /// Last confirmed physical cylinder.
    pub cylinder: u8,
    /// Last confirmed surface.
    pub side: Side,
    /// Drive mechanism type.
    pub drive_type: DriveType,
    /// Highest addressable cylinder count.
    pub max_cylinders: u8,
}

impl Default for DriveStatus {
    fn default() -> Self {
        Self {
            working: true,
            ready: false,
            disk_present: false,
            write_protected: true,
            motor_running: false,
            cylinder: 0,
            side: Side::Lower,
            drive_type: DriveType::Dd35,
            max_cylinders: 84,
        }
    }
}

/// Identifier assigned to an asynchronous write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WriteId(pub(crate) u64);

impl WriteId {
    /// Returns the bridge-local numeric identifier.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// An asynchronous write request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    /// Destination track.
    pub track: TrackAddress,
    /// Packed, most-significant-bit-first MFM words.
    pub words: Vec<u16>,
    /// Exact number of meaningful bits in `words`.
    pub bit_len: usize,
    /// Rotational bit position at which writing began.
    pub start_bit: usize,
}

/// An event produced by the worker.
#[derive(Debug)]
#[non_exhaustive]
pub enum BridgeEvent {
    /// The interface observed a disk insertion or removal.
    DiskChanged {
        /// Whether a disk is now present.
        present: bool,
    },
    /// A submitted write completed on the device.
    WriteCompleted {
        /// Write identifier.
        id: WriteId,
        /// Destination track.
        track: TrackAddress,
        /// Number of bits physically submitted.
        bit_len: usize,
    },
    /// A submitted write failed after it had been accepted.
    WriteFailed {
        /// Write identifier.
        id: WriteId,
        /// Destination track.
        track: TrackAddress,
        /// Failure.
        error: Error,
    },
    /// The interface disconnected or the worker stopped.
    Disconnected(Error),
}
