// Boot a firmware image in the WebAssembly build under Node.js and save the LCD.
// Usage: node tools/wasm-smoke.mjs module.wasm firmware.fwsc [steps] [out.png]
import { readFileSync, writeFileSync } from "node:fs";
import { deflateSync, crc32 } from "node:zlib";
import { instantiateFm1 } from "../web/fm1-host.js";

const [wasmPath, firmwarePath, stepsArg = "100000000", pngPath] = process.argv.slice(2);
const x = await instantiateFm1(readFileSync(wasmPath), { chaining: !process.env.NOCHAIN });
const mem = () => new Uint8Array(x.memory.buffer);
const message = () => new TextDecoder().decode(mem().slice(x.fm1_message_ptr(), x.fm1_message_ptr() + x.fm1_message_len()));

const firmware = readFileSync(firmwarePath);
const ptr = x.fm1_alloc(firmware.length);
mem().set(firmware, ptr);
if (x.fm1_load(ptr, firmware.length) !== 0) throw new Error(message());
if (process.env.NOJIT) x.fm1_set_jit(0);
if (process.env.VERIFY) x.fm1_set_jit_verify(1);
if (process.env.PROFILE_EXEC) x.fm1_profile_exec(1);

const total = Number(stepsArg);
const start = performance.now();
let status = 0;
let half = null; // { time, guest } once half the steps ran: steady-state speed excludes warm-up
const CHUNK = Number(process.env.CHUNK ?? 4_000_000);
for (let done = 0; done < total && status === 0; done += CHUNK) {
  if (!half && done >= total / 2) half = { time: performance.now(), guest: x.fm1_guest_seconds() };
  status = x.fm1_run(Math.min(CHUNK, total - done));
  // PROFILE_EXEC_AFTER=steps: count interpreter calls only after warm-up.
  if (process.env.PROFILE_EXEC_AFTER && done < Number(process.env.PROFILE_EXEC_AFTER) && done + 4_000_000 >= Number(process.env.PROFILE_EXEC_AFTER)) x.fm1_profile_exec(1);
}
const end = performance.now();
const seconds = (end - start) / 1000;
x.fm1_status();
const info = JSON.parse(message());
console.log(JSON.stringify({ status, seconds: +seconds.toFixed(2), mStepsPerSec: +(info.steps / 1e6 / seconds).toFixed(1), realtime: +(info.guestSeconds / seconds).toFixed(3),
  steadyRealtime: half ? +((info.guestSeconds - half.guest) * 1000 / (end - half.time)).toFixed(3) : null, ...info }));

if (process.env.PROFILE_EXEC || process.env.PROFILE_EXEC_AFTER) { x.fm1_profile_exec(0); console.log(message()); }
if (pngPath) {
  const lcdPtr = x.fm1_lcd();
  if (!lcdPtr) throw new Error("no LCD frame");
  const rgba = mem().slice(lcdPtr, lcdPtr + 240 * 240 * 4);
  const rows = [];
  for (let y = 0; y < 240; y++) {
    rows.push(Buffer.from([0]));
    rows.push(Buffer.from(rgba.subarray(y * 960, (y + 1) * 960)));
  }
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(Buffer.concat([Buffer.from(type), data])) >>> 0);
    return Buffer.concat([len, Buffer.from(type), data, crc]);
  };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(240, 0); ihdr.writeUInt32BE(240, 4); ihdr[8] = 8; ihdr[9] = 6;
  writeFileSync(pngPath, Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(Buffer.concat(rows))), chunk("IEND", Buffer.alloc(0))]));
}
