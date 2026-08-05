<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# Internals

## Ownership model

The `Bridge` handle contains bounded command and event channels, shared
read-only snapshots, and a join handle. A single worker exclusively owns the
device backend and transport. No unsafe `Send` implementation or cross-thread
hardware flag is required.

The worker processes queued commands, refreshes status at a bounded cadence,
and captures only when the motor and media are ready. Redundant seek requests
can be dropped when the command queue is saturated; writes and motor changes
are never silently dropped.

## Position invariant

The worker distinguishes the consumer's target track from its last confirmed
physical position. Background caching may temporarily change the latter. Every
write calls the same positioning routine immediately before the device write,
restoring and verifying cylinder and side. This is the central invariant that
prevents cached work from redirecting a write to another track.

## Capture cache

The cache is keyed by `TrackAddress`, holds at most two generations per track,
and has a bounded number of tracks. The active track is protected from normal
eviction. `advance_revolution` retires the front capture; the worker then fills
the vacancy with a later physical revolution.

Index-aligned streams are cut at device index markers. Immediate streams
capture more than one nominal revolution and search a bounded DD/HD window for
the strongest 1,024-cell overlap. AmigaDOS structure and checksums provide a
second, format-aware reusable-capture test.

## Flux path

Backends convert their wire format into nanosecond `FluxEvent`s. The dynamic
PLL tracks phase and frequency within a ±10% window and emits normalized 2 μs
Amiga bit cells. MFM writes take the reverse route: exact meaningful bits are
converted to transition intervals and then encoded in the selected device's
bounded stream format.

All stream parsers use absolute deadlines and maximum sizes. Arithmetic that
crosses bit, byte, tick, or nanosecond units is checked at the boundary.

## Shutdown and failure

Dropping `Bridge` enqueues shutdown and joins the worker. The worker turns off
the motor before releasing its device. Unexpected transport termination marks
the status unhealthy and emits a disconnect event. A normal disk/index timeout
does not kill the worker; the caller can continue polling or change media.
