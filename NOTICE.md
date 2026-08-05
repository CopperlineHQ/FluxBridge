<!-- SPDX-License-Identifier: LGPL-3.0-or-later -->

# Notices and provenance

FluxBridge is an independent Rust port of runtime code from:

- Project: FloppyDriveBridge
- Author and maintainer: Robert Smith (RobSmithDev)
- Repository: <https://github.com/RobSmithDev/FloppyDriveBridge>
- Port baseline: `710fa15cb200303f8c4bde1c931786175f301a68`
- Relevant later change: <https://github.com/RobSmithDev/FloppyDriveBridge/pull/15>

The original project describes its active sources as multi-licensed under
MPL-2.0 or GPL-2.0-or-later, with `ArduinoInterface` under
LGPL-3.0-or-later and its public ABI headers under the Unlicense. FluxBridge
selects MPL-2.0 for material ported from the multi-licensed sources and combines
that material with the LGPL-3.0-or-later work in this repository. File-level
SPDX identifiers record which terms apply.

The following Rust areas are substantially informed by or adapted from the
corresponding upstream implementation:

| FluxBridge area | Principal upstream source |
| --- | --- |
| Dynamic PLL and revolution extraction | `pll.*`, `RotationExtractor.*` |
| DrawBridge protocol and stream encoding | `ArduinoInterface.*` |
| Greaseweazle protocol and flux encoding | `GreaseWeazleInterface.*` |
| SuperCard Pro protocol and streaming | `SuperCardProInterface.*` |
| Controller behavior and cache invariants | `CommonBridgeTemplate.*` |

Greaseweazle protocol work upstream also credits Keir Fraser and the
Greaseweazle project. SuperCard Pro protocol work upstream credits Jim Drew and
CBMSTUFF.COM. Those acknowledgements are retained here.

FluxBridge is not affiliated with or endorsed by the original authors or
hardware vendors.
