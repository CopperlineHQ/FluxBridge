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
cargo test --features hardware-tests -- --ignored --nocapture
```

Inventory and read probes do not alter media. A write probe must additionally
set `FLUXBRIDGE_TEST_WRITE=1`; use only a disposable disk. Hardware tests never
fall back from the requested driver or port.

No physical interface was available during the initial port. Passing CI proves
the controller and protocol transcripts, not electrical compatibility with
every firmware/hardware combination.
