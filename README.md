<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# FluxBridge

FluxBridge is a safe Rust library for reading and writing physical floppy
drives through DrawBridge, Greaseweazle, and SuperCard Pro interfaces. It
provides typed discovery and configuration, non-blocking revolution capture,
and observable asynchronous writes without a C or C++ ABI.

This project is a Rust port of the active runtime portions of Rob Smith's
[FloppyDriveBridge][upstream]. The initial port is based on upstream commit
`710fa15cb200303f8c4bde1c931786175f301a68` and incorporates the Linux/musl
2 Mbaud correction from [upstream pull request #15][pr15]. See
[NOTICE.md](NOTICE.md) and [the porting notes](docs/porting.md) for provenance
and deliberate behavioral changes.

## Status

The public API, controller, flux algorithms, and all three device protocols are
implemented. Protocol and controller behavior is covered by deterministic
tests; hardware tests are opt-in because they require a physical interface and
real media. Treat the `0.1` API as young, and report the interface, firmware,
host OS, and port identifier with hardware issues.

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

`ReadMode::Fast` begins immediately and validates the reconstructed overlap.
`Compatible` captures between index pulses. `Stalling` has compatible capture
semantics but allows `read_track` to wait for at most the configured
`stall_timeout`. Writes are accepted with `submit_write`; actual completion or
failure arrives through `poll_event`.

Port enumeration deliberately does not probe every serial device. Use
`PortSelection::Auto` to probe candidates suitable for the chosen driver, or
persist the `PortId` returned by `ports()` and use `PortSelection::Exact`.

## Features

The default features are:

- `drawbridge`
- `greaseweazle`
- `supercard-pro`
- `direct-ftdi`, using the pure-Rust `ftdi-nusb` transport

Disable default features to build only the protocols an application needs.
The crate itself forbids unsafe Rust. The serial transport disables
`serialport`'s `libudev` feature, which keeps Linux musl builds self-contained.

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
