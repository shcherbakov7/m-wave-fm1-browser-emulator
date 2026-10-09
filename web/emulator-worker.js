// SPDX-License-Identifier: GPL-3.0-only
// Owns the WebAssembly emulator. Paces guest time against the host clock so
// the device runs at its real speed: each slice runs the guest up to "now"
// (bounded host time so input stays responsive), then sleeps if it is ahead.
// Streams LCD frames, audio (straight to the audio worklet) and status.
import { instantiateFm1 } from "./fm1-host.js";

const SLICE_MS = 8;           // host time per slice before yielding to messages
const MAX_LAG_S = 0.1;        // further behind than this: drop the backlog
const FRAME_MS = 16;          // LCD frame rate cap (~60 fps)
const STATUS_MS = 250;
const AUDIO_CHUNK = 4096;     // frames per audio read

let x = null;                 // wasm exports
let running = false;
let paused = false;
let stepsPerMs = 20_000;      // measured; bounds each slice to SLICE_MS
let clock = null;             // { wall, guest }: guest time due at wall time
let lastFrame = 0;
let lastStatus = 0;
let rate = { guest: 0, time: 0, value: 0, busy: 0, load: 0 };
let audioPtr = 0;
let serialPtr = 0;
let audioPort = null;         // MessagePort to the audio worklet
let timer = 0;

const mem = () => new Uint8Array(x.memory.buffer);
const message = () => {
  const ptr = x.fm1_message_ptr();
  return new TextDecoder().decode(mem().slice(ptr, ptr + x.fm1_message_len()));
};

async function init(wasmUrl) {
  const response = await fetch(wasmUrl);
  x = await instantiateFm1(await response.arrayBuffer());
  audioPtr = x.fm1_alloc(AUDIO_CHUNK * 2 * 4);
  serialPtr = x.fm1_alloc(4096);
  postMessage({ type: "ready" });
}

function resync() {
  clock = { wall: performance.now(), guest: x.fm1_guest_seconds() };
}

function status() {
  x.fm1_status();
  const info = JSON.parse(message());
  info.realtime = rate.value;
  info.load = rate.load;
  info.paused = paused;
  postMessage({ type: "status", info });
}

function load(bytes, name) {
  running = false;
  const ptr = x.fm1_alloc(bytes.byteLength);
  mem().set(new Uint8Array(bytes), ptr);
  if (x.fm1_load(ptr, bytes.byteLength) !== 0) {
    postMessage({ type: "error", message: message(), name });
    return;
  }
  rate = { guest: 0, time: performance.now(), value: 0, busy: 0, load: 0 };
  audioPort?.postMessage("clear");
  postMessage({ type: "loaded", name });
  running = true;
  paused = false;
  resync();
  schedule();
}

function flushOutputs(now) {
  if (now - lastFrame >= FRAME_MS) {
    const ptr = x.fm1_lcd();
    if (ptr) {
      const frame = mem().slice(ptr, ptr + 240 * 240 * 4);
      postMessage({ type: "lcd", frame }, [frame.buffer]);
    }
    lastFrame = now;
  }
  for (;;) {
    const frames = x.fm1_audio(audioPtr, AUDIO_CHUNK);
    if (!frames) break;
    if (audioPort) {
      const samples = new Float32Array(x.memory.buffer, audioPtr, frames * 2).slice();
      audioPort.postMessage(samples, [samples.buffer]);
    }
    if (frames < AUDIO_CHUNK) break;
  }
  const serial = x.fm1_serial(serialPtr, 4096);
  if (serial) postMessage({ type: "serial", text: new TextDecoder().decode(mem().slice(serialPtr, serialPtr + serial)) });
  if (now - lastStatus >= STATUS_MS) {
    status();
    lastStatus = now;
  }
}

function slice() {
  timer = 0;
  if (!running || paused) return;
  const start = performance.now();
  const guest = x.fm1_guest_seconds();
  let target = clock.guest + (start - clock.wall) / 1000;
  if (target - guest > MAX_LAG_S) {
    // Slower than the device right now: run on from here rather than
    // racing to catch up (which would play the backlog at the wrong speed).
    clock = { wall: start, guest };
    target = guest + SLICE_MS / 1000;
  }
  let result = 0;
  if (target > guest) {
    const steps = x.fm1_steps();
    result = x.fm1_run_until(target, Math.max(10_000, Math.min(50_000_000, stepsPerMs * SLICE_MS)));
    const elapsed = performance.now() - start;
    const ran = x.fm1_steps() - steps;
    // Learn the step rate only from slices that ran long enough to time.
    if (elapsed > 1 && ran > 0) stepsPerMs = Math.round(stepsPerMs * 0.7 + (ran / elapsed) * 0.3);
    rate.busy += elapsed;
  }
  const now = performance.now();
  if (now - rate.time >= 1000) {
    const guestNow = x.fm1_guest_seconds();
    // Guest seconds per host second: 1.0 is real time.
    rate.value = (guestNow - rate.guest) * 1000 / (now - rate.time);
    rate.load = rate.busy / (now - rate.time);
    rate.guest = guestNow;
    rate.time = now;
    rate.busy = 0;
  }
  flushOutputs(now);
  if (result === 1) {
    running = false;
    postMessage({ type: "fault", message: message() });
    status();
    return;
  }
  const due = clock.guest + (now - clock.wall) / 1000 - x.fm1_guest_seconds();
  if (due < 0) timer = setTimeout(slice, Math.min(SLICE_MS, -due * 1000));
  else schedule();
}

// MessageChannel yields to incoming messages without setTimeout's clamping.
const channel = new MessageChannel();
channel.port1.onmessage = slice;
function schedule() { channel.port2.postMessage(0); }
function wake() {
  if (timer) { clearTimeout(timer); timer = 0; }
  schedule();
}

onmessage =({ data }) => {
  switch (data.type) {
    case "init": init(data.wasmUrl).catch((error) => postMessage({ type: "error", message: String(error) })); break;
    case "load": load(data.bytes, data.name); break;
    case "key": if (x) x.fm1_key(data.id, data.down ? 1 : 0); break;
    case "audio-port": audioPort = data.port; break;
    case "pause":
      paused = data.paused;
      if (!paused && running) { resync(); wake(); }
      status();
      break;
    case "serial": {
      if (!x) break;
      const bytes = new TextEncoder().encode(data.text);
      const ptr = x.fm1_alloc(bytes.length);
      mem().set(bytes, ptr);
      const accepted = x.fm1_serial_in(ptr, bytes.length);
      x.fm1_free(ptr, bytes.length);
      if (!accepted) postMessage({ type: "serial-busy" });
      break;
    }
  }
};
