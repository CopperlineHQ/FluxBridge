<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# Public API

## Discovery

`drivers()` returns the protocols compiled into the crate. Each `DriverInfo`
has a stable `DriverKind` and typed `Capabilities`; persistent configuration
must use the kind rather than an enumeration index or display-name substring.

`ports()` returns system serial ports plus direct FTDI devices when that feature
is enabled. A `PortId` is the stable token accepted by
`PortSelection::Exact`. Enumeration is passive: a listed serial device has not
necessarily been identified as a floppy interface.

## Configuration and opening

`BridgeConfig` selects the driver, read mode, density, drive-select line, port,
auto-cache behavior, and stalling deadline. `validate()` rejects unsupported
drive selectors and missing compiled backends. `Bridge::open` probes only
driver-appropriate candidates in automatic mode; an exact port is always
honored and never replaced by another candidate.

An open `Bridge` owns one physical interface and is neither `Clone` nor backed
by a global singleton. Dropping it asks the worker to stop, turns the motor
off, joins the worker, and closes the transport.

## Status and positioning

`status()` returns a cached `DriveStatus` without device I/O. It includes worker
health, readiness, media and write-protect state, motor state, physical
position, mechanism type, and cylinder limit.

`set_motor`, `seek`, and `no_click_step` enqueue bounded commands. The requested
cylinder is clamped to the physical mechanism. Commands may return a queue-full
or worker-stopped error, but never wait for a serial transaction.

## Captures

`read_track` requests the track and returns `Ok(None)` until a complete
revolution is ready. `ReadMode::Stalling` is the sole exception: it may wait for
at most `stall_timeout`. A `TrackCapture` contains packed MSB-first `u16` words,
an exact bit length, a monotonic generation, and a `CaptureQuality`.

`IndexAligned` and `VerifiedAmigaDos` captures are safe to retain and replay.
`Unverified` means the immediate-mode overlap could not be proven as a seamless
AmigaDOS ring; serve it once, call `advance_revolution`, and request fresh flux
on retry. Custom formats are conservatively unverified rather than rejected.

## Writes and events

A `WriteRequest` owns the destination, packed words, exact meaningful bit
length, and rotational starting bit. `submit_write` checks capacity,
write-protection state, and whether a partial write can be safely placed. It
returns a `WriteId` only when the bounded worker queue accepts the request.

Acceptance is not physical success. Poll `poll_event()` for:

- `WriteCompleted`
- `WriteFailed`
- `DiskChanged`
- `Disconnected`

The worker invalidates the destination cache after every attempted write.
Before touching media it restores both requested cylinder and side, even if
automatic caching moved the drive after the write was queued.

## Errors

`Error::kind()` maps detailed errors to stable `ErrorKind` categories suitable
for UI and telemetry. Timeouts are operation-specific and do not automatically
mean disconnection. Protocol framing failures, invalid firmware, permission
errors, port ownership, and unsafe write placement remain distinguishable.
