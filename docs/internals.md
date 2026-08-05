<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# Internals

## Ownership model

The `Bridge` handle contains latest-wins desired state, a bounded command
channel for the few real commands (writes, the no-click step, shutdown), a
bounded event channel, shared read-only snapshots, and a join handle. A single
worker exclusively owns the device backend and transport. No unsafe `Send`
implementation or cross-thread hardware flag is required.

Motor state and head position are wishes, not commands: the caller overwrites
a shared `Desired` record and the worker applies whatever it holds at the top
of every loop. An emulated machine expresses these thousands of times a
second -- every guest step pulse, every CIA motor write -- and only the latest
state can matter to a physical mechanism. Wishes therefore cannot fail, cannot
be lost, and cannot fall behind; writes and motor transitions are never
silently dropped. A nudge channel wakes the worker the moment a wish changes
so it is not held for the tail of a poll interval.

## Position and demand

The worker distinguishes three coordinates. The *target* follows the
machine's stepper wherever it goes, including every cylinder passed through
mid-seek. The *wanted* track is the one a consumer actually asked to read --
the only place worth spending an uninterruptible capture window. A capture
begins only when the stepper's destination is the wanted track, so a head
being walked somewhere is never dragged back against its own journey. The
*physical* position is the last confirmed device state; every write restores
both requested cylinder and side against it immediately before the device
write, which is the invariant that prevents capture activity from redirecting
a write to another track.

## Capture cache

The cache is keyed by `TrackAddress`, holds at most two generations per
track, and has a bounded number of tracks with the wanted track protected
from eviction. Depth is demand-driven: a track whose newest capture is
reusable keeps one; only a track whose newest capture cannot be replayed
stocks a second, so the retry is already in hand when the consumer asks.
`advance_revolution` retires the front capture and the worker refills the
vacancy with a later physical revolution.

Index-aligned streams are cut at device index markers. Immediate streams
capture more than one nominal revolution and prove the join by pattern
matching: anchors are taken past the PLL warm-up at several spread offsets,
each is searched for its strongest recurrence, and the winning revolution
length must be confirmed by a majority of anchors. AmigaDOS structure and
checksums provide a second, format-aware reusable-capture test; a capture
that fails both is served once and replaced rather than replayed.

While an immediate capture is arriving, the device republishes the decode so
far after each received chunk. The worker exposes that growing prefix through
a shared partial-capture slot, which the finished revolution then withdraws.

## Flux path

Backends convert their wire format into nanosecond `FluxEvent`s. The dynamic
PLL tracks phase and frequency within a ±10% window and emits normalized 2 μs
Amiga bit cells; high-density flux is presented to the same PLL at doubled
intervals. MFM writes take the reverse route: exact meaningful bits are
converted to transition intervals and then encoded in the selected device's
bounded stream format.

All stream parsers use absolute deadlines and maximum sizes. Arithmetic that
crosses bit, byte, tick, or nanosecond units is checked at the boundary.

## Shutdown and failure

Dropping `Bridge` enqueues shutdown and joins the worker. The worker turns off
the motor before releasing its device. Transient device errors are weather --
an overflowed capture, a read racing a motor toggle -- and are only fatal when
they run unbroken past a generous ceiling; a positively disconnected transport
is fatal immediately, marks the status unhealthy, and emits a disconnect
event. A normal disk/index timeout does not kill the worker; the caller can
continue polling or change media.
