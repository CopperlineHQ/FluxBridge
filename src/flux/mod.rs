// SPDX-License-Identifier: MPL-2.0 AND LGPL-3.0-or-later

//! Pure flux, PLL, and Amiga MFM processing.
//!
//! These algorithms do not perform I/O and are deterministic, making them
//! suitable for image importers and diagnostics as well as bridge backends.

mod pll;
mod rotation;
mod scan;

pub use pll::{FluxEvent, PllDecoder};
pub use rotation::{
    MAX_TRACK_BITS, bits_to_unaligned_revolution, flux_to_revolution, flux_to_unaligned_revolution,
    mfm_to_flux, pack_bits, rotate_revolution,
};
pub use scan::{RevolutionScan, scan_revolution};
