# Licensing and provenance

This project is distributed under **GPL-3.0-only** (see [LICENSE](LICENSE)).

## Emulator core (`emulator/core`)

The pi32v2 interpreter and FM-1 board model are imported from the Rust
implementation of [simonjohansson/fm1-emulator](https://github.com/simonjohansson/fm1-emulator)
(commit `a119dc5`, the last revision before that project switched to QEMU),
which is GPL-3.0-only. The upstream project's research notes are kept in
`emulator/core/*.md`.

Upstream credits, kept as published there:

- [Felucca](https://github.com/hugelton/Felucca) (GPL-3.0) — research and panel defaults.
- [JieLi AC79 SDK](https://gitee.com/Jieli-Tech/fw-AC79_AIoT_SDK) (Apache-2.0, see
  [LICENSES/JieLi-SDK-Apache-2.0.txt](LICENSES/JieLi-SDK-Apache-2.0.txt)) — register
  and package-format evidence.
- [Quarkslab pi32v2 SLEIGH reference](https://github.com/quarkslab/ghidra-jieli) (Apache-2.0) —
  instruction encodings.

## Test fixtures (`emulator/fixtures`)

Small diagnostic firmware images built by the upstream project for its own
tests. They are emulator test inputs and must not be flashed to a device.

## Firmware

No third-party FM-1 firmware is included. Users load their own `.fwsc` files;
each firmware keeps its own license.
