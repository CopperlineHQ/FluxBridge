<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# FluxBridge

A Rust library for using a real floppy drive as an emulated machine's drive.
The emulator asks for tracks; FluxBridge runs the interface hardware on a
worker thread and hands back decoded MFM revolutions. Reads never block the
caller, a track can be served while the disk is still turning past the head,
and writes are queued and reported back when the hardware finishes them.

Written for [Copperline](https://github.com/CopperlineHQ/Copperline), but the
API has no Copperline types in it and any emulator can use it. The crate is
pure Rust and forbids unsafe code.

## Hardware support

| Interface | State |
|---|---|
| [Greaseweazle](https://github.com/keirf/greaseweazle) | Supported. Tested on real hardware on macOS, Windows, and Linux hosts; tuned for emulator use. Needs main firmware 0.27 or newer. |
| DrawBridge | Protocol implemented behind the `drawbridge` feature. Not yet tested on hardware. |
| SuperCard Pro | Protocol implemented behind the `supercard-pro` feature. Not yet tested on hardware. |

`drivers()` lists the drivers compiled into a build, so an application's
interface menu can be built from it rather than hardcoded. Copperline
enables `greaseweazle` only.

## Design

- Motor and seek requests are latest-wins state, not queued commands. An
  emulator can send them thousands of times a second; only the newest
  matters to the mechanism, and none are lost.
- `read_track` returns `None` until a capture is ready. During an immediate
  capture, `partial_track` returns the decoded revolution so far, so the
  consumer can serve the early sectors while the later ones are still under
  the head.
- `ReadMode` selects the capture strategy: `Normal` captures immediately
  without waiting for the index pulse, `Compatible` captures from one index
  pulse to the next, `Stalling` is index-aligned and may block the caller up
  to `stall_timeout`.
- An immediate capture is cut where the recording repeats, and the cut is
  verified: the pattern must match at several offsets, and the result is
  also scanned as AmigaDOS structure with checksums. Every capture carries a
  `CaptureQuality` saying whether it is safe to replay or should be served
  once and re-read.
- `DensityMode::Auto` senses DD or HD from the media: DrawBridge asks its
  firmware, Greaseweazle measures the flux intervals (it has no sense
  line). The result also sets the density of later `Auto` writes.
- The head steps at 3 ms, the rate an Amiga steps its own drive.
- `submit_write` returns a `WriteId`; completion or failure arrives through
  `poll_event`. Write-protected disks, and writes the hardware cannot place
  where they were asked for, are refused.

## Example

```rust,no_run
use fluxbridge::{Bridge, BridgeConfig, DriverKind, Side, TrackAddress};

let mut bridge = Bridge::open(&BridgeConfig {
    driver: DriverKind::Greaseweazle,
    ..BridgeConfig::default()
})?;
let track = TrackAddress { cylinder: 0, side: Side::Lower };

bridge.set_motor(track.side, true)?;
bridge.seek(track)?;
if let Some(capture) = bridge.read_track(track)? {
    println!("{} bits, {:?}", capture.bit_len(), capture.quality());
}
# Ok::<(), fluxbridge::Error>(())
```

## Origin

FluxBridge started as a Rust port of Rob Smith's
[FloppyDriveBridge](https://github.com/RobSmithDev/FloppyDriveBridge). The
device protocols and the PLL come from that port. The controller and capture
pipeline were then redesigned and no longer follow the original: motor and
seek handling, capture policy, streaming, and revolution verification all
work differently. [NOTICE.md](NOTICE.md) records the provenance;
[docs/porting.md](docs/porting.md) records what was changed and why.

## Documentation

[API guide](docs/api.md) · [internals](docs/internals.md) ·
[protocols](docs/protocols.md) · [porting notes](docs/porting.md) ·
[testing](docs/testing.md), or `cargo doc --all-features --no-deps`.

## Acknowledgements

Rob Smith (FloppyDriveBridge), Keir Fraser (Greaseweazle), Jim Drew /
CBMSTUFF.COM (SuperCard Pro).

## Licensing

`LGPL-3.0-or-later AND MPL-2.0`. Newly written and LGPL-derived modules are
`LGPL-3.0-or-later`; modules derived from FloppyDriveBridge are
`MPL-2.0 AND LGPL-3.0-or-later` (FloppyDriveBridge is multi-licensed and the
MPL option was taken). Each file carries an SPDX header; full texts are in
[`LICENSES`](LICENSES/).
