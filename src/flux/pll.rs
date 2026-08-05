// SPDX-License-Identifier: MPL-2.0 AND LGPL-3.0-or-later

//! Dynamic software PLL.
//!
//! This is a safe Rust adaptation of FloppyDriveBridge's `BridgePLL`. It uses
//! an authentic, gradual phase correction rather than snapping the window to
//! every transition.

const CLOCK_CENTRE_NS: i64 = 2_000;
const CLOCK_MIN_NS: i64 = 1_800;
const CLOCK_MAX_NS: i64 = 2_200;

/// One measured delay ending in a magnetic flux transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluxEvent {
    /// Delay since the previous transition, in nanoseconds.
    pub nanoseconds: u32,
    /// Whether the index pulse occurred at this transition.
    pub index: bool,
}

/// Stateful dynamic PLL converting flux delays to MFM bit cells.
#[derive(Debug, Clone)]
pub struct PllDecoder {
    clock_ns: i64,
    pending_ns: i64,
}

impl Default for PllDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PllDecoder {
    /// Creates a PLL centred on the Amiga DD 2 μs bit cell.
    pub const fn new() -> Self {
        Self {
            clock_ns: CLOCK_CENTRE_NS,
            pending_ns: 0,
        }
    }

    /// Resets clock and phase state.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Submits a transition delay and appends decoded cells to `output`.
    ///
    /// The transition itself is represented by a final `true` cell, preceded
    /// by the decoded number of zero cells.
    pub fn submit(&mut self, nanoseconds: u32, output: &mut Vec<bool>) {
        self.pending_ns = self.pending_ns.saturating_add(i64::from(nanoseconds));
        if self.pending_ns < self.clock_ns / 2 {
            return;
        }

        let zeros = ((self.pending_ns - self.clock_ns / 2) / self.clock_ns).max(0);
        self.pending_ns -= (zeros + 1) * self.clock_ns;

        if (1..=3).contains(&zeros) {
            self.clock_ns += (self.pending_ns / (zeros + 1)) / 10;
        } else {
            self.clock_ns += (CLOCK_CENTRE_NS - self.clock_ns) / 10;
        }
        self.clock_ns = self.clock_ns.clamp(CLOCK_MIN_NS, CLOCK_MAX_NS);

        // Do not snap all the way to the transition: retain half the phase
        // mismatch, matching the inertia of a real separator.
        self.pending_ns /= 2;
        output.extend(std::iter::repeat_n(
            false,
            usize::try_from(zeros).unwrap_or_default(),
        ));
        output.push(true);
    }

    /// Returns the current estimated bit-cell duration.
    pub const fn clock_nanoseconds(&self) -> i64 {
        self.clock_ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ideal_mfm_intervals_decode() {
        let mut pll = PllDecoder::new();
        let mut bits = Vec::new();
        for interval in [4_000, 6_000, 8_000] {
            pll.submit(interval, &mut bits);
        }
        assert_eq!(
            bits,
            [false, true, false, false, true, false, false, false, true]
        );
    }

    #[test]
    fn clock_adjustment_is_bounded() {
        let mut pll = PllDecoder::new();
        let mut bits = Vec::new();
        for _ in 0..10_000 {
            pll.submit(4_400, &mut bits);
        }
        assert!((CLOCK_MIN_NS..=CLOCK_MAX_NS).contains(&pll.clock_nanoseconds()));
    }
}
