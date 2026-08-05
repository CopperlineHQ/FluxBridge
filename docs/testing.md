<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# Testing

## Normal validation

Run:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo doc --all-features --no-deps
```

The deterministic suite covers PLL behavior, indexed and immediate revolution
extraction, rotated AmigaDOS rings, exact bit lengths, DrawBridge encodings,
Greaseweazle interval forms, SuperCard Pro packets and firmware parsing,
nonblocking capture, desired-state motor and seek under command flood, and
the capture-moved-drive wrong-track write regression.

CI additionally builds without default features, checks Linux musl, and runs
the suite on Linux, macOS, and Windows.

## Hardware tests

Hardware tests are ignored unless the `hardware-tests` feature and explicit
environment are provided:

```text
FLUXBRIDGE_TEST_DRIVER=greaseweazle \
FLUXBRIDGE_TEST_PORT=/dev/ttyACM0 \
FLUXBRIDGE_TEST_DRIVE=pc-a \
cargo test --features hardware-tests -- --ignored --nocapture
```

Inventory and read probes do not alter media. A write probe must additionally
set `FLUXBRIDGE_TEST_WRITE=1`; use only a disposable disk. Hardware tests never
fall back from the requested driver, port, or drive-select line. Supported
drive tokens are `pc-a`, `pc-b`, and `shugart-0` through `shugart-3`; the
default is `pc-a`.

The Greaseweazle path has been validated extensively on physical hardware.
The original port validation captured both sides at cylinders 0, 1, 40, and
79 from an AmigaTestKit DD disk on a Shugart-select-0 drive, every sampled
revolution decoding as 11 clean AmigaDOS sectors. The reworked controller was
then validated end-to-end under Copperline on an IBM PC Drive A cable:
repeated Workbench 1.3 boots to the desktop from the physical disk under
Kickstart 1.3, all three read modes exercised, streamed partial serving
active, whole boots completing without a single failed join reconstruction,
and guest-initiated writes laid on a real disk and read back correctly --
on macOS, Windows 11, and Linux hosts. Passing CI still proves controller and protocol behaviour
rather than electrical compatibility with every firmware and drive
combination.
