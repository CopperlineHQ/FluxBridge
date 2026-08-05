// SPDX-License-Identifier: MPL-2.0 AND LGPL-3.0-or-later

//! Hardware protocol backends.

#![cfg_attr(
    not(any(
        feature = "drawbridge",
        feature = "greaseweazle",
        feature = "supercard-pro"
    )),
    allow(dead_code, unused_variables)
)]

use std::time::Duration;

use crate::flux::MAX_TRACK_BITS;
use crate::{
    BridgeConfig, DensityMode, DriveSelect, DriveStatus, DriverKind, Error, PortId, PortInfo,
    PortSelection, ReadMode, Result, TrackAddress,
};

#[cfg(feature = "drawbridge")]
mod drawbridge;
#[cfg(feature = "greaseweazle")]
mod greaseweazle;
#[cfg(feature = "supercard-pro")]
mod supercard_pro;

pub(crate) struct RawCapture {
    pub words: Vec<u16>,
    pub bit_len: usize,
    pub index_aligned: bool,
}

pub(crate) trait Device: Send {
    fn selected_port(&self) -> &PortId;
    fn status(&mut self) -> Result<DriveStatus>;
    fn set_motor(&mut self, enabled: bool, quick: bool) -> Result<()>;
    fn seek(&mut self, cylinder: u8) -> Result<()>;
    fn select_side(&mut self, side: crate::Side) -> Result<()>;
    fn no_click_step(&mut self) -> Result<()>;
    fn read_track(
        &mut self,
        mode: ReadMode,
        density: DensityMode,
        progress: &mut dyn FnMut(Vec<u16>, usize),
    ) -> Result<RawCapture>;
    fn write_track(
        &mut self,
        words: &[u16],
        bit_len: usize,
        from_index: bool,
        density: DensityMode,
    ) -> Result<()>;
}

pub(crate) fn open(config: &BridgeConfig) -> Result<Box<dyn Device>> {
    config.validate()?;
    let candidates = candidates(config)?;
    if candidates.is_empty() {
        return Err(Error::PortNotFound(format!(
            "no candidate ports are visible for {}",
            config.driver
        )));
    }

    let exact = matches!(config.port, PortSelection::Exact(_));
    let mut last_error = None;
    for port in candidates {
        let opened = match config.driver {
            #[cfg(feature = "drawbridge")]
            DriverKind::DrawBridge => drawbridge::DrawBridge::open(port.id.clone(), config),
            #[cfg(feature = "greaseweazle")]
            DriverKind::Greaseweazle => greaseweazle::Greaseweazle::open(port.id.clone(), config),
            #[cfg(feature = "supercard-pro")]
            DriverKind::SuperCardPro => supercard_pro::SuperCardPro::open(port.id.clone(), config),
            #[allow(unreachable_patterns)]
            _ => Err(Error::InvalidConfig(format!(
                "{} support is not compiled in",
                config.driver
            ))),
        };
        match opened {
            Ok(device) => return Ok(device),
            Err(error) if exact => return Err(error),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        Error::PortNotFound(format!("no {} interface responded", config.driver))
    }))
}

fn candidates(config: &BridgeConfig) -> Result<Vec<PortInfo>> {
    if let PortSelection::Exact(id) = &config.port {
        return Ok(vec![PortInfo {
            id: id.clone(),
            display_name: id.to_string(),
            vid: None,
            pid: None,
            serial_number: None,
            product: None,
        }]);
    }

    let mut ports = crate::ports()?;
    ports.sort_by_key(|port| std::cmp::Reverse(port_score(config.driver, port)));
    ports.retain(|port| port_score(config.driver, port) > 0);
    Ok(ports)
}

fn port_score(driver: DriverKind, port: &PortInfo) -> u8 {
    let product = port
        .product
        .as_deref()
        .unwrap_or(&port.display_name)
        .to_ascii_lowercase();
    match driver {
        DriverKind::Greaseweazle => {
            if port.vid == Some(0x1209) && port.pid == Some(0x4d69)
                || product.contains("greaseweazle")
            {
                100
            } else {
                0
            }
        }
        DriverKind::SuperCardPro => {
            if product.contains("supercard") || product.contains("scp-jim") {
                100
            } else {
                0
            }
        }
        DriverKind::DrawBridge => {
            if port.id.is_direct_ftdi() {
                90
            } else if product.contains("greaseweazle")
                || product.contains("supercard")
                || matches!((port.vid, port.pid), (Some(0x1a86), Some(0x7523)))
            {
                0
            } else if port.vid.is_some() {
                20
            } else {
                5
            }
        }
    }
}

pub(crate) fn validate_capture(capture: &RawCapture) -> Result<()> {
    if capture.bit_len < 64 || capture.bit_len > capture.words.len() * 16 {
        return Err(Error::Protocol(format!(
            "backend returned invalid {}-bit capture in {} words",
            capture.bit_len,
            capture.words.len()
        )));
    }
    if capture.bit_len > MAX_TRACK_BITS {
        return Err(Error::TrackTooLarge {
            bits: capture.bit_len,
            limit: MAX_TRACK_BITS,
        });
    }
    Ok(())
}

pub(crate) fn operation_deadline() -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(2)
}

pub(crate) fn clamp_track(track: TrackAddress, max_cylinders: u8) -> TrackAddress {
    TrackAddress {
        cylinder: track.cylinder.min(max_cylinders.saturating_sub(1)),
        side: track.side,
    }
}

pub(crate) fn drive_selection(config: DriveSelect) -> (u8, u8) {
    match config {
        DriveSelect::PcA => (1, 0),
        DriveSelect::PcB => (1, 1),
        DriveSelect::Shugart0 => (2, 0),
        DriveSelect::Shugart1 => (2, 1),
        DriveSelect::Shugart2 => (2, 2),
        DriveSelect::Shugart3 => (2, 3),
    }
}
