# M-VAVE FM-1 emulator in the browser

**English** · [Русский](README.ru.md)

**Open the emulator:** https://shcherbakov7.github.io/m-wave-fm1-browser-emulator/

An emulator of the **M-VAVE FM-1** synthesizer that runs **unmodified
firmware** (`.fwsc`, `.ufw`, and `.bin`/`.elf`) right in the browser. It
emulates the JieLi AC791N processor (pi32v2 instruction set, two cores) and
the board's peripherals: the 240×240 screen, the 27 keys and 14 buttons,
audio, USB and flash memory. The core is written in Rust and built to
WebAssembly.

![The emulator panel running Felucca](docs/panel.png)

![Eight firmwares in the emulator](docs/compatibility.png)

## Compatibility

Run for 1.5 billion instructions without errors, drawing their interface on
the screen (in the WebAssembly build with the JIT):

| Firmware | Status |
|---|---|
| Official V13, V14, V15 (`FM-1.fwsc`) | boots, screen, sound, buttons |
| Felucca 1.1.5.1 | boots, interface, sound, USB console, buttons |
| SLOOP 2.4.1 | boots to the splash screen and beyond |
| SLOOP ALG-02 (ALG05-TEST) | boots to the splash screen and beyond |
| X0X 0.10.3-beta | boots, sequencer interface |
| Melodee 0.13.1 | boots to the splash screen and beyond |

Each site build also boots every firmware it offers in the emulator and shows
the result on the page. Not tested (no files): Baud Girl FM-1+VA, Groove OS
(paid) and others. If a firmware stops, the emulator shows the address and
the reason. Please send them in an issue.

## How to use

1. Open the emulator page (GitHub Pages, link above) or run it locally.
2. Pick a firmware in the **Choose firmware…** list or the firmware card. It
   offers the latest builds of the open-source firmwares (Felucca, SLOOP, X0X,
   Melodee, FoMni, Jangada, ChoralRoot, FiMba, fm1-nes, SLOOP ALG) and the
   official M-VAVE firmware. A link like `…/?fw=felucca` opens that firmware
   straight away. Open your own `.fwsc` with **Own file** or drop it on the
   panel; it goes into the "Your files" group.

   The site build takes the open-source firmwares (GPL-3.0) from their
   authors' repositories (`tools/update-firmware.mjs`), checks each one in the
   emulator (`tools/check-firmware.mjs`; the result is shown on the page) and
   refreshes them daily. The official firmware is proprietary, so the site
   does not host it but downloads it from the M-VAVE server; if the server
   does not allow that, the page links to the file.
3. The panel mirrors the FM-1: all 14 buttons, 27 keys and 8 knobs work.
   Press buttons and keys with the mouse or by touch (you can slide a finger
   along the keys). Turn knobs by dragging up/down, with the mouse wheel, or
   with the arrow keys after Tab. MASTER is the volume (a potentiometer);
   SELECT, PRESETS, ALGORITHM and KNOB 1–4 are encoders, as on the device.
   Button and key lights follow the LEDs. Computer keyboard: `Z`…`/` and
   `S D F H J L ;` are the keys from F3, `1 Q…Y 3 4 6` the upper part,
   `←`/`→` OCT−/OCT+, `Esc` HOME, Space PLAY/STOP.
4. **Sound** turns audio output on. Firmware you open is remembered in the
   browser (the recent list).
5. The interface language (English or Russian) is switched with **EN / RU**
   in the header and remembered; a link with `?lang=ru` opens the Russian
   version.

## Limitations

- **Speed.** The emulator keeps pace with real time and never runs ahead of
  the device; if the computer cannot keep up, emulation slows down (the
  "% of real time" figure under the screen; its tooltip shows the load).
  Measured in Chromium on a slow server CPU (Xeon 2.1 GHz, one core): SLOOP
  ≈ 50%, Felucca ≈ 52%, official V15 ≈ 42% of real speed. A modern desktop
  CPU should manage about twice that. The official firmware is the heaviest:
  it serves the button and LED matrix with an interrupt every 1–2 SPI bytes
  (tens of thousands of interrupts per second).
- USB MIDI from the computer, Bluetooth and firmware updates "over the air"
  (through the emulated updater) are not supported.
- Cases not confirmed on hardware (float division by zero and the like) use
  standard IEEE 754 behaviour.
- If a browser tab crashes while a firmware runs, the next visit steps down
  to a lighter engine mode (no block chaining, then the interpreter only) and
  says so under the stats; `?jit=1` goes back to the full mode.

## Building and running locally

You need Rust (with the `wasm32-unknown-unknown` target) and Python or
Node.js for a static server:

```sh
rustup target add wasm32-unknown-unknown
tools/build-web.sh                  # builds web/fm1.wasm
node tools/update-firmware.mjs      # optional: fetch the open-source firmwares
python3 -m http.server -d web 8080  # then open http://localhost:8080
```

Tests and tools:

```sh
cargo test --release --manifest-path emulator/Cargo.toml
# Native firmware diagnosis: where it stopped, what is on the screen
cargo run --release --manifest-path emulator/Cargo.toml --example diagnose -- FIRMWARE.fwsc 300000000
FM1_LCD_PPM=screen.ppm cargo run --release ... --example diagnose -- FIRMWARE.fwsc 300000000
# Speed and instruction mix
PROFILE_OPS=1 cargo run --release --manifest-path emulator/Cargo.toml --example profile -- FIRMWARE.fwsc
# Check in a real Chromium (npm install for Playwright);
# SETTLE=40 keeps running 40 s after boot and measures the speed again
node tools/browser-smoke.mjs FIRMWARE.fwsc screenshot.png
# The WebAssembly build in Node: speed, screen, JIT verification
node tools/wasm-smoke.mjs web/fm1.wasm FIRMWARE.fwsc 1000000000 screen.png
# Knobs, buttons and LEDs: in Node and in Chromium
node tools/panel-check.mjs web/fm1.wasm FIRMWARE.fwsc
node tools/browser-panel.mjs FIRMWARE.fwsc screenshot.png
# Where a firmware spends its time, what code the JIT generates
cargo run --release --manifest-path emulator/Cargo.toml --example hotspots -- FIRMWARE.fwsc
```

## Publishing on GitHub Pages

The `.github/workflows/ci.yml` workflow runs the tests, builds WebAssembly,
fetches and checks the firmwares, and publishes the site from the default
branch (and daily, to pick up new firmware releases). Enable it once in the
repository settings: **Settings → Pages → Source: GitHub Actions**.

## Layout

```
web/                     page, Web Worker with the emulator, AudioWorklet, translations (i18n.js)
emulator/core/           core: pi32v2 CPU, bus, FM-1 peripherals, .fwsc loader
emulator/wasm/           C-ABI wrapper of the core for the browser
emulator/fixtures/       small test firmwares from upstream
tools/                   build, firmware catalog, checks in Node and Chromium
```

The emulator runs in a Web Worker and is tied to the clock: each slice (up to
8 ms) brings device time up to the present, and if the emulator is ahead it
waits. Between slices it handles input and sends screen frames (up to 60 fps),
while sound goes straight from the worker to an AudioWorklet with about 40 ms
of latency.

The speed comes from a JIT: hot firmware code (from flash and from SRAM) is
translated into WebAssembly modules that the browser compiles to machine
code. Regions of up to 16 basic blocks keep registers in local variables,
skip flags that are overwritten right away, and pass control to each other
with tail calls without returning to the dispatcher. Anything that touches
peripherals runs in the interpreter. Every translation is checked against
the interpreter in differential tests (`cargo test`), and
`VERIFY=1 node tools/wasm-smoke.mjs …` checks blocks on a real firmware.

While a core waits (idles until an interrupt, polls a timer, or spins in a
loop that returns to the same state), device time moves faster ("time
warp"); interrupts still arrive no later than 100 µs.

## License and credits

GPL-3.0-only, see [LICENSE](LICENSE) and [LICENSES.md](LICENSES.md).

The core is based on the Rust emulator from
[simonjohansson/fm1-emulator](https://github.com/simonjohansson/fm1-emulator)
(GPL-3.0). New instructions and fixes were checked against the vendor's
disassembler from [AL-255/FM-1-RE](https://github.com/AL-255/FM-1-RE), the
SLEIGH description in [quarkslab/ghidra-jieli](https://github.com/quarkslab/ghidra-jieli)
and the sources of [Felucca](https://github.com/hugelton/Felucca),
[SLOOP](https://github.com/isod89/sloop-fm1) and
[X0X](https://github.com/charlesvestal/fm1-x0x).
