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
nonblocking capture, and the automatic-cache wrong-track write regression.

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

The Greaseweazle path has also been validated on physical hardware: a
Greaseweazle attached to a Shugart-select-0 PC drive captured both sides at
cylinders 0, 1, 40, and 79 from an AmigaTestKit DD disk. Every sampled
revolution decoded as 11 clean AmigaDOS sectors, and Copperline booted the disk
through its Paula bitstream path under Kickstart 1.3. Passing CI still proves
controller and protocol behaviour rather than electrical compatibility with
every firmware and drive combination.
