// SPDX-License-Identifier: MPL-2.0 AND LGPL-3.0-or-later

//! Revolution extraction and MFM/flux conversion.

use crate::{Error, Result};

use super::{FluxEvent, PllDecoder};

/// Maximum bounded MFM track length accepted by the crate.
pub const MAX_TRACK_BITS: usize = 0x3a00 * 2 * 8;

/// Packs a bit stream into most-significant-bit-first words.
pub fn pack_bits(bits: &[bool]) -> Vec<u16> {
    let mut words = vec![0_u16; bits.len().div_ceil(16)];
    for (position, bit) in bits.iter().copied().enumerate() {
        if bit {
            words[position / 16] |= 1 << (15 - position % 16);
        }
    }
    words
}

/// Rotates a packed revolution left by `start_bit`.
pub fn rotate_revolution(words: &[u16], bit_len: usize, start_bit: usize) -> Result<Vec<u16>> {
    if bit_len == 0 || bit_len > words.len() * 16 {
        return Err(Error::Protocol("invalid packed revolution length".into()));
    }
    let start_bit = start_bit % bit_len;
    let bits = (0..bit_len)
        .map(|offset| {
            let position = (start_bit + offset) % bit_len;
            words[position / 16] & (1 << (15 - position % 16)) != 0
        })
        .collect::<Vec<_>>();
    Ok(pack_bits(&bits))
}

/// Decodes flux events between two index pulses into one packed revolution.
///
/// Events before the first index establish PLL phase but are not returned.
/// If the stream contains no index pulse, all decoded events form the result;
/// callers must then treat the capture as unverified.
pub fn flux_to_revolution(events: &[FluxEvent]) -> Result<(Vec<u16>, usize, bool)> {
    let mut pll = PllDecoder::new();
    let mut warmup = Vec::new();
    let mut bits = Vec::new();
    let has_index = events.iter().any(|event| event.index);
    let mut capturing = !has_index;
    let mut saw_start = false;

    for event in events {
        if event.index && saw_start {
            break;
        }
        if event.index {
            saw_start = true;
            capturing = true;
            bits.clear();
        }
        let output = if capturing { &mut bits } else { &mut warmup };
        pll.submit(event.nanoseconds, output);
        if bits.len() > MAX_TRACK_BITS {
            return Err(Error::TrackTooLarge {
                bits: bits.len(),
                limit: MAX_TRACK_BITS,
            });
        }
    }
    if bits.len() < 64 {
        return Err(Error::Protocol(
            "flux stream did not contain a complete revolution".into(),
        ));
    }
    let bit_len = bits.len();
    Ok((pack_bits(&bits), bit_len, saw_start))
}

/// Decodes an immediate flux capture and locates its repeated revolution join.
///
/// # Errors
///
/// Returns an error when the capture is too short, too large, or contains no
/// sufficiently convincing repeated boundary.
pub fn flux_to_unaligned_revolution(
    events: &[FluxEvent],
    high_density: bool,
) -> Result<(Vec<u16>, usize)> {
    let mut pll = PllDecoder::new();
    let mut bits = Vec::new();
    for event in events {
        pll.submit(event.nanoseconds, &mut bits);
        if bits.len() > MAX_TRACK_BITS {
            return Err(Error::TrackTooLarge {
                bits: bits.len(),
                limit: MAX_TRACK_BITS,
            });
        }
    }
    let bits = bits_to_unaligned_revolution(&bits, high_density)?;
    let bit_len = bits.len();
    Ok((pack_bits(&bits), bit_len))
}

/// Locates a repeated revolution boundary in an immediate decoded bit stream.
///
/// The capture must contain at least one revolution plus an overlap. Candidate
/// joins are searched around the physical DD or HD revolution length and
/// scored over 1,024 cells, as in the original rotation extractor.
///
/// # Errors
///
/// Returns an error if no repeated boundary is sufficiently convincing.
pub fn bits_to_unaligned_revolution(bits: &[bool], high_density: bool) -> Result<Vec<bool>> {
    let nominal = if high_density { 200_000 } else { 100_000 };
    let lower = nominal * 4 / 5;
    let upper = (nominal * 6 / 5).min(bits.len().saturating_sub(1_024));
    if upper <= lower || bits.len() < lower + 1_024 {
        return Err(Error::Protocol(
            "immediate capture is too short to locate a revolution overlap".into(),
        ));
    }
    let sample = 1_024;
    let mut best = (0_usize, 0_usize);
    for candidate in lower..=upper {
        let score = bits[..sample]
            .iter()
            .zip(&bits[candidate..candidate + sample])
            .filter(|(left, right)| left == right)
            .count();
        if score > best.1 {
            best = (candidate, score);
        }
    }
    if best.1 < sample * 3 / 4 {
        return Err(Error::Protocol(format!(
            "could not locate a reliable revolution overlap (best score {}/{sample})",
            best.1
        )));
    }
    Ok(bits[..best.0].to_vec())
}

/// Converts packed MFM to transition delays for flux-writing interfaces.
///
/// A final partial word is truncated at `bit_len`; checked arithmetic prevents
/// the byte/bit unit confusion present in the original write buffer.
pub fn mfm_to_flux(words: &[u16], bit_len: usize, cell_nanoseconds: u32) -> Result<Vec<u32>> {
    if bit_len == 0 || bit_len > words.len() * 16 {
        return Err(Error::InvalidConfig(format!(
            "bit length {bit_len} does not fit {} MFM words",
            words.len()
        )));
    }
    if bit_len > MAX_TRACK_BITS {
        return Err(Error::TrackTooLarge {
            bits: bit_len,
            limit: MAX_TRACK_BITS,
        });
    }
    let mut flux = Vec::with_capacity(bit_len / 2);
    let mut cells = 0_u32;
    for position in 0..bit_len {
        cells = cells
            .checked_add(1)
            .ok_or_else(|| Error::InvalidConfig("flux interval overflow".into()))?;
        if words[position / 16] & (1 << (15 - position % 16)) != 0 {
            flux.push(cells.checked_mul(cell_nanoseconds).ok_or_else(|| {
                Error::InvalidConfig("flux interval nanoseconds overflow".into())
            })?);
            cells = 0;
        }
    }
    if cells != 0 {
        flux.push(
            cells
                .checked_mul(cell_nanoseconds)
                .ok_or_else(|| Error::InvalidConfig("trailing flux interval overflow".into()))?,
        );
    }
    Ok(flux)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_crosses_word_boundary() {
        let words = [0x8001, 0x4000];
        let rotated = rotate_revolution(&words, 32, 15).unwrap();
        assert_eq!(rotated, vec![0xa000, 0x4000]);
    }

    #[test]
    fn write_conversion_honours_exact_bit_length() {
        let flux = mfm_to_flux(&[0x9000], 4, 2_000).unwrap();
        assert_eq!(flux, vec![2_000, 6_000]);
    }

    #[test]
    fn indexed_flux_extracts_one_revolution() {
        let events = [
            FluxEvent {
                nanoseconds: 4_000,
                index: false,
            },
            FluxEvent {
                nanoseconds: 4_000,
                index: true,
            },
        ];
        let mut events = events.to_vec();
        events.extend((0..40).map(|_| FluxEvent {
            nanoseconds: 4_000,
            index: false,
        }));
        events.push(FluxEvent {
            nanoseconds: 4_000,
            index: true,
        });
        let (_, bits, indexed) = flux_to_revolution(&events).unwrap();
        assert!(indexed);
        assert!(bits >= 64);
    }

    #[test]
    fn repeated_immediate_capture_finds_its_join() {
        let mut state = 0x1234_5678_u32;
        let revolution = (0_usize..100_003)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state & 1 != 0
            })
            .collect::<Vec<_>>();
        let mut capture = revolution.clone();
        capture.extend_from_slice(&revolution[..20_000]);
        assert_eq!(
            bits_to_unaligned_revolution(&capture, false).unwrap(),
            revolution
        );
    }
}
