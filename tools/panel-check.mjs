// Check panel inputs and LEDs in the WebAssembly build under Node.js: boots a
// firmware, prints the lit LEDs, turns each encoder and reports whether the
// screen changed. Usage: node tools/panel-check.mjs module.wasm firmware.fwsc [boot-steps]
import { readFileSync } from "node:fs";
import { instantiateFm1 } from "../web/fm1-host.js";

const [wasmPath, firmwarePath, bootArg = "1500000000"] = process.argv.slice(2);
const x = await instantiateFm1(readFileSync(wasmPath));
const mem = () => new Uint8Array(x.memory.buffer);
const firmware = readFileSync(firmwarePath);
const ptr = x.fm1_alloc(firmware.length);
mem().set(firmware, ptr);
if (x.fm1_load(ptr, firmware.length) !== 0) throw new Error("load failed");
const NAMES = ["OCT-", "OCT+", "FX", "SEL", "ENV", "LFO", "EDIT", "GLO", "HOME", "SAVE", "ARP", "SEQ", "PLAY", "REC",
  ...Array.from({ length: 27 }, (_, i) => `key${i}`)];
const ENCODERS = ["SELECT", "PRESETS", "ALGORITHM", "KNOB1", "KNOB2", "KNOB3", "KNOB4"];
const run = (seconds) => { const t = x.fm1_guest_seconds() + seconds; while (x.fm1_guest_seconds() < t) if (x.fm1_run_until(t, 50_000_000)) throw new Error("fault"); };
const leds = () => { const p = x.fm1_leds(); return [...mem().slice(p, p + 41)].map((v, i) => v ? `${NAMES[i]}:${v}` : "").filter(Boolean).join(" "); };
let frame = null;
const screen = () => { const p = x.fm1_lcd(); if (p) frame = mem().slice(p, p + 240 * 240 * 4); return frame; };
const differs = (a, b) => { let n = 0; for (let i = 0; i < a.length; i += 4) if (a[i] !== b[i] || a[i + 1] !== b[i + 1]) n++; return n; };

for (let done = 0; done < Number(bootArg); done += 50_000_000) if (x.fm1_run(50_000_000)) throw new Error("fault at boot");
run(0.5); leds(); run(0.3);
console.log("LEDs after boot:", leds() || "(none)");
for (let i = 0; i < ENCODERS.length; i++) {
  run(0.3); const before = screen().slice();
  x.fm1_encoder(i, 3); run(0.6);
  const changed = differs(before, screen());
  x.fm1_encoder(i, -3); run(0.6);
  console.log(`${ENCODERS[i]}: ${changed} pixels changed after 3 detents`);
}
x.fm1_key(12, 1); run(0.2); x.fm1_key(12, 0); run(0.5);
console.log("LEDs after PLAY:", leds() || "(none)");
