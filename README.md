<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# FluxBridge

A safe, pure-Rust library for reading and writing real floppy drives from an
emulator. **Greaseweazle** is the supported, hardware-validated interface; it
drives Workbench from a physical disk under
[Copperline](https://github.com/CopperlineHQ/Copperline) at speeds comparable
to a real Amiga's own drive. Copperline is the flagship consumer, but the API
carries nothing Copperline-specific: any emulator can embed it.

FluxBridge began as a Rust port of Rob Smith's [FloppyDriveBridge][upstream]
(baseline `710fa15c`, with the fix from [upstream PR #15][pr15]; provenance in
[NOTICE.md](NOTICE.md)). The runtime has since been redesigned around what an
emulated machine actually asks of a drive.

## What it does differently

- **Pure Rust, no C++ ABI.** A Cargo dependency with `#![forbid(unsafe_code)]`
  — nothing to install, nothing to dlopen.
- **Motor and seek are latest-wins state, not queued commands.** An emulator
  expresses them thousands of times a second; upstream's queue model can lose
  a motor transition under that load, and losing one leaves the controller's
  view of the drive permanently wrong.
- **Captures stream.** `partial_track` serves a revolution while the platter
  is still turning it, so the consumer reads the early sectors behind the
  head exactly as a real controller does, instead of waiting for the capture
  to finish.
- **Capture is demand-driven.** Upstream's background caching of neighbouring
  tracks was measured over a Workbench boot — right about once in twenty, at
  a full capture window of dead time each — and removed.
- **The index-less join is proved, and the verdict is yours.** Anchors past
  the PLL warm-up, majority vote across several offsets, plus an AmigaDOS
  structure-and-checksum scan; every capture carries a `CaptureQuality`
  (index-aligned / verified / unverified) so the consumer knows what is safe
  to replay and what to serve once.
- **Density is sensed from the flux itself** on Greaseweazle
  (`DensityMode::Auto`) — DD has no legal interval under 4 µs, so DD and HD
  cannot be confused. The verdict sticks, so an `Auto` write lands at the
  density the disk last read as.
- **The head steps at 3 ms**, the rate an Amiga's own stepper runs at, not
  the interface's 10 ms default.
- **Writes are observable.** `submit_write` returns a `WriteId`; completion
  or failure arrives as an event, not a fire-and-forget boolean.

DrawBridge and SuperCard Pro protocols are fully ported and compile behind
their own feature gates, but are not yet validated on hardware — bringing one
up is a testing exercise, not a redesign. `drivers()` reports exactly what a
build compiled in, most mature first, so an embedder's interface menu follows
the build; Copperline compiles `greaseweazle` alone.

## Use

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

`read_track` never blocks: `None` means "not yet", and `partial_track` serves
the capture in flight. `ReadMode::Fast` captures immediately and proves the
join; `Compatible` captures index-to-index, closed by construction;
`Stalling` may hold the caller up to `stall_timeout`. Ports are enumerated
without probing; use `PortSelection::Exact` to pin one, or `Auto` to probe
candidates suitable for the chosen driver.

Reads are non-destructive. Writes alter real media: FluxBridge refuses
write-protected disks and writes it cannot place safely, but applications
should still gate writes on explicit user permission and test with
disposable media first.

## Documentation

[API guide](docs/api.md) · [internals](docs/internals.md) ·
[protocols](docs/protocols.md) · [porting notes](docs/porting.md) ·
[testing](docs/testing.md) — or `cargo doc --all-features --no-deps`.

## Acknowledgements

- **Rob Smith** — [FloppyDriveBridge][upstream], the architecture and
  protocol knowledge this began from.
- **Keir Fraser** — [Greaseweazle](https://github.com/keirf/greaseweazle),
  the interface FluxBridge is tuned for.
- **Jim Drew / CBMSTUFF.COM** — the SuperCard Pro.
- **Lee Hobson** — the hardware validation: the real-disk boots, A/B timings,
  and listening sessions that shaped the controller into what ships here.

## Licensing

`LGPL-3.0-or-later AND MPL-2.0` at the package level: newly written and
LGPL-derived modules carry `LGPL-3.0-or-later`; modules substantially derived
from upstream carry `MPL-2.0 AND LGPL-3.0-or-later` (the MPL option was
selected for the upstream portion rather than GPL). SPDX headers identify
each file's terms; full texts are in [`LICENSES`](LICENSES/).

[upstream]: https://github.com/RobSmithDev/FloppyDriveBridge
[pr15]: https://github.com/RobSmithDev/FloppyDriveBridge/pull/15
