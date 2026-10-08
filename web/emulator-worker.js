// SPDX-License-Identifier: GPL-3.0-only
// Owns the WebAssembly emulator. Runs guest steps in time-boxed slices so
// input messages stay responsive, and streams LCD frames, audio and status.
"use strict";

const SLICE_MS = 12;          // CPU time per slice before yielding to messages
const FRAME_MS = 33;          // LCD frame rate cap (~30 fps)
const STATUS_MS = 250;
const AUDIO_CHUNK = 4096;     // frames per audio transfer

let x = null;                 // wasm exports
let running = false;
let paused = false;
let stepsPerSlice = 200_000;  // adapted to SLICE_MS
let lastFrame = 0;
let lastStatus = 0;
let rate = { steps: 0, time: 0, value: 0 };
let audioPtr = 0;
let serialPtr = 0;

const mem = () => new Uint8Array(x.memory.buffer);
const message = () => {
  const ptr = x.fm1_message_ptr();
  return new TextDecoder().decode(mem().slice(ptr, ptr + x.fm1_message_len()));
};

async function init(wasmUrl) {
  const response = await fetch(wasmUrl);
  const { instance } = await WebAssembly.instantiate(await response.arrayBuffer());
  x = instance.exports;
  audioPtr = x.fm1_alloc(AUDIO_CHUNK * 2 * 4);
  serialPtr = x.fm1_alloc(4096);
  postMessage({ type: "ready" });
}

function status() {
  x.fm1_status();
  const info = JSON.parse(message());
  info.stepsPerSecond = rate.value;
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
  rate = { steps: 0, time: performance.now(), value: 0 };
  postMessage({ type: "loaded", name });
  running = true;
  paused = false;
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
    const samples = new Float32Array(x.memory.buffer, audioPtr, frames * 2).slice();
    postMessage({ type: "audio", samples }, [samples.buffer]);
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
  if (!running || paused) return;
  const start = performance.now();
  const result = x.fm1_run(stepsPerSlice);
  const now = performance.now();
  const elapsed = Math.max(now - start, 0.1);
  // Aim each slice at SLICE_MS of host time.
  stepsPerSlice = Math.max(10_000, Math.min(20_000_000, Math.round(stepsPerSlice * SLICE_MS / elapsed)));
  if (now - rate.time >= 1000) {
    x.fm1_status();
    const steps = JSON.parse(message()).steps;
    rate.value = (steps - rate.steps) * 1000 / (now - rate.time);
    rate = { steps, time: now, value: rate.value };
  }
  flushOutputs(now);
  if (result === 1) {
    running = false;
    postMessage({ type: "fault", message: message() });
    status();
    return;
  }
  schedule();
}

// MessageChannel yields to incoming messages without setTimeout's clamping.
const channel = new MessageChannel();
channel.port1.onmessage = slice;
function schedule() { channel.port2.postMessage(0); }

onmessage = ({ data }) => {
  switch (data.type) {
    case "init": init(data.wasmUrl).catch((error) => postMessage({ type: "error", message: String(error) })); break;
    case "load": load(data.bytes, data.name); break;
    case "key": if (x) x.fm1_key(data.id, data.down ? 1 : 0); break;
    case "pause":
      paused = data.paused;
      if (!paused && running) schedule();
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
