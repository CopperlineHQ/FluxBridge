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
support.

Flux opcodes and compact intervals are converted with the firmware-reported
sample clock. Reads and disk probes have hard deadlines. Motor-enable failures,
missing index pulses, overflow, underflow, and write protection are propagated
rather than converted to success.

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
