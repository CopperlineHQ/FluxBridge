// SPDX-License-Identifier: MPL-2.0 AND LGPL-3.0-or-later

//! Format-aware verification of an index-less revolution boundary.

use std::collections::HashSet;

const MASK: u32 = 0x5555_5555;
const DD_SECTORS: usize = 11;
const HD_SECTORS: usize = 22;
const HD_BIT_THRESHOLD: usize = 150_000;

/// Classification of a captured revolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevolutionScan {
    /// A complete AmigaDOS track whose header and data checksums pass.
    CleanAmigaDos {
        /// Sectors decoded: 11 for DD or 22 for HD.
        sectors: u8,
    },
    /// Recognisable but incomplete or corrupt AmigaDOS data.
    DamagedAmigaDos {
        /// Distinct sectors whose header and data passed.
        good: u8,
        /// Sector count implied by density.
        expected: u8,
    },
    /// Insufficient AmigaDOS structure to judge; this includes custom formats.
    Unrecognised,
}

#[inline]
fn bit_at(words: &[u16], bit_len: usize, position: usize) -> bool {
    let position = position % bit_len;
    words[position / 16] & (1 << (15 - position % 16)) != 0
}

fn long_at(words: &[u16], bit_len: usize, start: usize) -> u32 {
    (0..32).fold(0, |value, offset| {
        (value << 1) | u32::from(bit_at(words, bit_len, start + offset))
    })
}

#[inline]
const fn deinterleave(odd: u32, even: u32) -> u32 {
    ((odd & MASK) << 1) | (even & MASK)
}

/// Decodes all AmigaDOS sectors in a packed revolution, treating its ends as
/// joined, and verifies both header and data checksums.
pub fn scan_revolution(words: &[u16], bit_len: usize) -> RevolutionScan {
    if bit_len < 64 || words.is_empty() || words.len() * 16 < bit_len {
        return RevolutionScan::Unrecognised;
    }

    let mut syncs = HashSet::new();
    let mut window = 0_u16;
    for position in 0..bit_len + 15 {
        window = (window << 1) | u16::from(bit_at(words, bit_len, position));
        if position >= 15 && window == 0x4489 {
            syncs.insert((position - 15) % bit_len);
        }
    }

    let mut headers_valid = 0_usize;
    let mut good = [false; HD_SECTORS];
    for &sync in &syncs {
        if !syncs.contains(&((sync + 16) % bit_len))
            || syncs.contains(&((sync + bit_len - 16) % bit_len))
        {
            continue;
        }
        let body = sync + 32;
        let info_odd = long_at(words, bit_len, body);
        let info_even = long_at(words, bit_len, body + 32);
        let [format, _track, sector, _to_gap] = deinterleave(info_odd, info_even).to_be_bytes();
        if format != 0xff || usize::from(sector) >= HD_SECTORS {
            continue;
        }

        let mut header_checksum = (info_odd & MASK) ^ (info_even & MASK);
        for offset in 0..8 {
            header_checksum ^= long_at(words, bit_len, body + 64 + offset * 32) & MASK;
        }
        let stored_header = deinterleave(
            long_at(words, bit_len, body + 320),
            long_at(words, bit_len, body + 352),
        );
        if header_checksum != stored_header {
            continue;
        }
        headers_valid += 1;

        let data_start = body + 448;
        let data_checksum = (0..256).fold(0, |checksum, offset| {
            checksum ^ (long_at(words, bit_len, data_start + offset * 32) & MASK)
        });
        let stored_data = deinterleave(
            long_at(words, bit_len, body + 384),
            long_at(words, bit_len, body + 416),
        );
        if data_checksum == stored_data {
            good[usize::from(sector)] = true;
        }
    }

    let expected = if bit_len > HD_BIT_THRESHOLD {
        HD_SECTORS
    } else {
        DD_SECTORS
    };
    let good = good.into_iter().filter(|valid| *valid).count();
    if good == expected {
        RevolutionScan::CleanAmigaDos {
            sectors: u8::try_from(expected).unwrap_or_default(),
        }
    } else if headers_valid + 1 >= expected {
        RevolutionScan::DamagedAmigaDos {
            good: u8::try_from(good).unwrap_or_default(),
            expected: u8::try_from(expected).unwrap_or_default(),
        }
    } else {
        RevolutionScan::Unrecognised
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flux::{pack_bits, rotate_revolution};

    fn push_long(bits: &mut Vec<bool>, value: u32) {
        bits.extend((0..32).rev().map(|bit| value & (1 << bit) != 0));
    }

    fn encode_sector(track: u8, sector: u8) -> Vec<bool> {
        let mut bits = Vec::new();
        bits.extend((0..16).rev().map(|bit| 0x2aaa & (1 << bit) != 0));
        bits.extend((0..16).rev().map(|bit| 0x4489 & (1 << bit) != 0));
        bits.extend((0..16).rev().map(|bit| 0x4489 & (1 << bit) != 0));
        let info = u32::from_be_bytes([0xff, track, sector, 1]);
        let odd = (info >> 1) & MASK;
        let even = info & MASK;
        push_long(&mut bits, odd);
        push_long(&mut bits, even);
        for _ in 0..8 {
            push_long(&mut bits, 0);
        }
        let header_checksum = odd ^ even;
        push_long(&mut bits, (header_checksum >> 1) & MASK);
        push_long(&mut bits, header_checksum & MASK);
        push_long(&mut bits, 0);
        push_long(&mut bits, 0);
        for _ in 0..256 {
            push_long(&mut bits, 0);
        }
        bits
    }

    fn track() -> (Vec<u16>, usize) {
        let mut bits = Vec::new();
        for sector in 0..11 {
            bits.extend(encode_sector(40, sector));
        }
        bits.extend(std::iter::repeat_n(false, 1_400));
        (pack_bits(&bits), bits.len())
    }

    #[test]
    fn clean_and_rotated_tracks_verify() {
        let (words, bits) = track();
        assert_eq!(
            scan_revolution(&words, bits),
            RevolutionScan::CleanAmigaDos { sectors: 11 }
        );
        let rotated = rotate_revolution(&words, bits, bits / 3).unwrap();
        assert_eq!(
            scan_revolution(&rotated, bits),
            RevolutionScan::CleanAmigaDos { sectors: 11 }
        );
    }

    #[test]
    fn malformed_lengths_are_unrecognised() {
        assert_eq!(scan_revolution(&[0xffff], 32), RevolutionScan::Unrecognised);
    }
}
