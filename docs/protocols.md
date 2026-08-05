<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# Hardware protocols

## DrawBridge

DrawBridge uses 2,000,000 baud with RTS/CTS and firmware 1.8 or newer. Opening
synchronizes on the version signature, pulses DTR/RTS once if needed, reads
firmware features, and rewinds. DD and HD streaming formats are decoded
directly. Writes use the firmware's packed transition format with bounded
precompensation on inner DD cylinders.

The serial transport configures the port atomically before normal blocking I/O.
This both avoids a Unix carrier-detect open hang and delegates Linux 2 Mbaud
selection to `serialport`, including its termios2/BOTHER and musl mappings.
Direct FTDI identifiers use `ftdi-nusb`; no proprietary D2XX library is loaded.

## Greaseweazle

Greaseweazle uses its framed command protocol at 9,600 baud and requires main
firmware 0.27 or newer. Opening reads the 32-byte firmware record, resets the
board, reads drive delays, selects IBM PC or Shugart bus mode, and verifies pin
support. The head-step interval is set to 3 ms -- the rate an Amiga's own
stepper runs at -- in place of the interface's 10 ms default. Disk change is
sampled from pin 34 on the IBM PC bus; the Shugart bus cannot sample it, so
media is presumed present there and change detection is simulated.

Flux opcodes and compact intervals are converted with the firmware-reported
sample clock. Immediate reads capture a fixed 232 ms window with no index
bound and decode it incrementally, publishing the track-so-far after each
received chunk; index-aligned reads bound the capture by index count and by a
tick ceiling, so a platter that stops mid-capture ends the read instead of
hanging it. `DensityMode::Auto` measures the raw intervals of each capture to
decide density, since the hardware has no sense line. Reads and disk probes
have hard deadlines. Motor-enable failures, missing index pulses, overflow,
underflow, and write protection are propagated rather than converted to
success; a flux-buffer overflow is retried over a purged pipe before it is
reported, since it is host scheduling weather rather than a fault of the
disk.

## SuperCard Pro

SuperCard Pro uses checksummed command packets at 9,600 baud and requires
firmware 1.3 or newer. Firmware and hardware versions are parsed as nibbles and
compared lexicographically. The exact requested serial port is used for every
open.

Realtime stream parsing recognizes index markers and 8-bit delay extension.
The worker itself reads and stops the stream, removing the original startup
race between a boolean flag and a background reader. Writes load bounded
big-endian 16-bit flux counters into device RAM before issuing `WRITEFLUX`.

## Compatibility boundaries

FluxBridge implements only runtime drive access needed by Copperline. It does
not provide the upstream DLL ABI, GUI profile editor, string profile format,
network update checker, or ADF test bridge. Stable Rust enums replace driver
indices and capability bit arithmetic in applications.
