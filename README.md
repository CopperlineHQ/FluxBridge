<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# FluxBridge

FluxBridge is a safe Rust library for reading and writing physical floppy
drives through Greaseweazle, DrawBridge, and SuperCard Pro interfaces. It
provides typed discovery and configuration, non-blocking revolution capture
that can stream a track while the platter is still turning it, and observable
asynchronous writes without a C or C++ ABI.

The project began as a Rust port of the active runtime portions of Rob
Smith's [FloppyDriveBridge][upstream], based on upstream commit
`710fa15cb200303f8c4bde1c931786175f301a68` with the Linux/musl 2 Mbaud
correction from [upstream pull request #15][pr15]. The controller and capture
pipeline have since been substantially reworked around what an emulated
machine actually asks of a drive; see [NOTICE.md](NOTICE.md) and
[the porting notes](docs/porting.md) for provenance and deliberate
behavioral changes.

## Status

Greaseweazle is the supported and optimized driver: it is validated on real
hardware, drives Workbench from a physical disk under the
[Copperline](https://github.com/CopperlineHQ/Copperline) emulator at speeds
comparable to a real Amiga's own drive, and is tuned for that use --
immediate index-less capture, streamed partial revolutions, and a head step
rate matched to the machine driving it.

DrawBridge and SuperCard Pro are fully ported and compile behind their own
feature gates, but have not yet been validated on physical hardware. The
driver table, capability flags, and per-driver validation are structured so
that bringing one up is a testing exercise, not a redesign. Reports from
hardware owners are welcome: include the interface, firmware, host OS, and
port identifier.

## Use

```rust,no_run
use fluxbridge::{
    Bridge, BridgeConfig, DriverKind, Side, TrackAddress,
};

let config = BridgeConfig {
    driver: DriverKind::Greaseweazle,
    ..BridgeConfig::default()
};
let mut bridge = Bridge::open(&config)?;
let track = TrackAddress {
    cylinder: 0,
    side: Side::Lower,
};

bridge.set_motor(track.side, true)?;
bridge.seek(track)?;
if let Some(capture) = bridge.read_track(track)? {
    println!(
        "{} bits, {:?}, reusable={}",
        capture.bit_len(),
        capture.quality(),
        capture.quality().reusable(),
    );
}
# Ok::<(), fluxbridge::Error>(())
```

Motor and seek requests are latest-wins state, not queued commands: an
emulator can express them thousands of times a second and the worker moves
the mechanism to wherever it belongs *now*. `read_track` never blocks; while
a capture is still arriving, `partial_track` returns the revolution as far
as the head has read it, so a consumer can serve the early sectors at the
platter's own pace.

`ReadMode::Fast` captures immediately without waiting for the index pulse
and proves the reconstructed overlap before a capture is marked reusable.
`Compatible` captures between index pulses, which closes the revolution by
construction. `Stalling` has compatible capture semantics but allows
`read_track` to wait for at most the configured `stall_timeout`. Writes are
accepted with `submit_write`; actual completion or failure arrives through
`poll_event`.

`DensityMode::Auto` resolves the media density per driver: DrawBridge asks
its firmware, and Greaseweazle measures the raw flux intervals themselves --
double-density MFM has no legal interval under 4 µs, so the two densities
cannot be confused. The verdict sticks, so an `Auto` write lands at the
density the disk last read as.

Port enumeration deliberately does not probe every serial device. Use
`PortSelection::Auto` to probe candidates suitable for the chosen driver, or
persist the `PortId` returned by `ports()` and use `PortSelection::Exact`.

## Features

The default features are:

- `greaseweazle`
- `drawbridge`
- `supercard-pro`
- `direct-ftdi`, using the pure-Rust `ftdi-nusb` transport

Disable default features to build only the protocols an application needs;
`drivers()` reports exactly what was compiled in, most mature first, so an
embedder's interface menu can be built from it rather than hardcoded.
Copperline builds with `greaseweazle` alone. The crate itself forbids unsafe
Rust. The serial transport disables `serialport`'s `libudev` feature, which
keeps Linux musl builds self-contained.

## Safety

Reads are non-destructive. Writes alter real media and may be impossible to
undo. FluxBridge rejects write-protected disks, oversized buffers, inconsistent
bit lengths, and partial writes that the selected hardware cannot place
safely. Applications should still require explicit user permission before
enabling writes and should test with disposable media first.

## Documentation

- [Public API guide](docs/api.md)
- [Internal architecture](docs/internals.md)
- [Hardware protocols](docs/protocols.md)
- [Porting notes and fixed defects](docs/porting.md)
- [Testing and hardware checks](docs/testing.md)

API reference can also be generated with `cargo doc --all-features --no-deps`.

## Acknowledgements

- **Rob Smith** wrote [FloppyDriveBridge][upstream], the architecture and
  protocol knowledge this project started from.
- **Keir Fraser** created [Greaseweazle](https://github.com/keirf/greaseweazle),
  the interface FluxBridge is tuned for and tested against.
- **Jim Drew / CBMSTUFF.COM** created the SuperCard Pro.
- **Lee Hobson** drove the hardware validation: the many real-disk boots,
  A/B timings, and listening sessions against a physical drive that shaped
  the controller, capture, and streaming behavior into what ships here.

## Licensing

FluxBridge is distributed under `LGPL-3.0-or-later AND MPL-2.0` at the package
level. Newly written and LGPL-derived modules carry
`LGPL-3.0-or-later`; modules substantially derived from upstream's
MPL/GPL-licensed implementation carry
`MPL-2.0 AND LGPL-3.0-or-later`. The MPL option was selected for the upstream
portion rather than GPL. SPDX headers identify the terms for each file, and
complete texts are in [`LICENSES`](LICENSES/).

[upstream]: https://github.com/RobSmithDev/FloppyDriveBridge
[pr15]: https://github.com/RobSmithDev/FloppyDriveBridge/pull/15
