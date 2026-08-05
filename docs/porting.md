<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# Porting notes

## Source basis

The port began from FloppyDriveBridge commit
`710fa15cb200303f8c4bde1c931786175f301a68`. Attribution and file mapping are in
[`NOTICE.md`](../NOTICE.md). The Rust implementation is organized around
ownership and protocol boundaries rather than mirroring the C++ class tree.

## Deliberate corrections

- Linux and musl 2 Mbaud setup incorporates the intent of upstream PR #15
  through `serialport` instead of passing a raw numeric speed to `cfsetspeed`.
- Serial ports are configured during nonblocking open and held exclusively;
  ignored lock failures and carrier-detect hangs are removed.
- One worker owns all mutable hardware state, eliminating unsynchronized
  booleans and the SuperCard Pro stream-start race.
- Every physical write restores both target cylinder and side after automatic
  cache activity.
- Write enqueue and write completion are distinct API events; backend failures
  are observable.
- SuperCard Pro honors exact port selection, returns a missing-port error,
  parses revision nibbles with `0x0f`, and compares firmware tuples correctly.
- DrawBridge firmware comparisons are lexicographic, so versions such as 0.9
  cannot satisfy a 1.8 minimum.
- Greaseweazle motor failures and flux-marker timeouts propagate as errors.
- Stream loops use fresh absolute deadlines rather than reusing mutated
  `select()` structures.
- Track limits consistently use bits at the API and bytes only on wire
  boundaries. Exact padding bits are accounted for and checked.
- Front-erased vectors and unbounded queues are replaced by bounded channels
  and `VecDeque`.

## API differences

There is no C ABI, raw driver handle, driver index, mutable global profile, or
string error buffer. Rust ownership closes the interface, typed configuration
rejects unsupported selectors, and `CaptureQuality` makes revolution
reusability explicit.

Automatic writes return `WriteId`; completion comes through `BridgeEvent`.
This is intentionally incompatible with the upstream boolean API, which could
only report queue acceptance.
