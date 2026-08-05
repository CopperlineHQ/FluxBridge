// SPDX-License-Identifier: LGPL-3.0-or-later

//! Error types.

use std::io;

/// A FluxBridge result.
pub type Result<T> = std::result::Result<T, Error>;

/// Stable, non-exhaustive categories suitable for status displays and events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The requested configuration is inconsistent or unsupported.
    InvalidConfig,
    /// The selected port does not exist.
    PortNotFound,
    /// Another process already owns the selected port.
    PortInUse,
    /// The process cannot access the selected port.
    PermissionDenied,
    /// The interface firmware is too old or incompatible.
    UnsupportedFirmware,
    /// The interface returned an invalid response.
    Protocol,
    /// An operation exceeded its deadline.
    Timeout,
    /// The interface was disconnected.
    Disconnected,
    /// The disk is physically write-protected.
    WriteProtected,
    /// A partial write cannot be placed safely.
    UnplaceablePartialWrite,
    /// A track exceeds the supported bounded buffer size.
    TrackTooLarge,
    /// The bridge worker stopped unexpectedly.
    WorkerStopped,
}

/// An error returned by FluxBridge.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Invalid or unsupported configuration.
    #[error("invalid bridge configuration: {0}")]
    InvalidConfig(String),
    /// The selected port was not found.
    #[error("port not found: {0}")]
    PortNotFound(String),
    /// The selected port is already in use.
    #[error("port is already in use: {0}")]
    PortInUse(String),
    /// The selected port cannot be accessed.
    #[error("permission denied opening port: {0}")]
    PermissionDenied(String),
    /// The connected firmware cannot implement the requested operation.
    #[error("unsupported firmware: {0}")]
    UnsupportedFirmware(String),
    /// The device returned an invalid response.
    #[error("device protocol error: {0}")]
    Protocol(String),
    /// An operation exceeded its deadline.
    #[error("operation timed out: {0}")]
    Timeout(&'static str),
    /// The device was disconnected.
    #[error("device disconnected")]
    Disconnected,
    /// The inserted disk is write-protected.
    #[error("the inserted disk is write-protected")]
    WriteProtected,
    /// The interface cannot safely place a partial write at the requested bit.
    #[error("partial write at bit {start_bit} cannot be safely placed on a {track_bits}-bit track")]
    UnplaceablePartialWrite {
        /// Requested starting bit.
        start_bit: usize,
        /// Known track length.
        track_bits: usize,
    },
    /// The requested track exceeds the bounded transfer size.
    #[error("track contains {bits} bits; the limit is {limit}")]
    TrackTooLarge {
        /// Requested bit count.
        bits: usize,
        /// Maximum bit count.
        limit: usize,
    },
    /// The worker thread stopped unexpectedly.
    #[error("bridge worker stopped")]
    WorkerStopped,
    /// A transport-level I/O failure.
    #[error("transport I/O error: {0}")]
    Io(#[from] io::Error),
}

impl Error {
    /// Returns the stable category for this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidConfig(_) => ErrorKind::InvalidConfig,
            Self::PortNotFound(_) => ErrorKind::PortNotFound,
            Self::PortInUse(_) => ErrorKind::PortInUse,
            Self::PermissionDenied(_) => ErrorKind::PermissionDenied,
            Self::UnsupportedFirmware(_) => ErrorKind::UnsupportedFirmware,
            Self::Protocol(_) => ErrorKind::Protocol,
            Self::Timeout(_) => ErrorKind::Timeout,
            Self::Disconnected => ErrorKind::Disconnected,
            Self::WriteProtected => ErrorKind::WriteProtected,
            Self::UnplaceablePartialWrite { .. } => ErrorKind::UnplaceablePartialWrite,
            Self::TrackTooLarge { .. } => ErrorKind::TrackTooLarge,
            Self::WorkerStopped => ErrorKind::WorkerStopped,
            Self::Io(error) => match error.kind() {
                io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
                io::ErrorKind::NotFound => ErrorKind::PortNotFound,
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ErrorKind::Timeout,
                io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::UnexpectedEof => ErrorKind::Disconnected,
                _ => ErrorKind::Protocol,
            },
        }
    }
}
