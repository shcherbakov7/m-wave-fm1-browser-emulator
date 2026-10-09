// SPDX-License-Identifier: GPL-3.0-only
// Plays interleaved stereo frames that the emulator worker posts straight to
// this processor (over a MessagePort handed in through `port`). Latency is
// kept low: playback starts once START frames are queued and, should the
// queue grow past LIMIT (host and audio clocks drift apart), the oldest
// frames are dropped back to START. An underrun plays silence and re-arms.
const START = 1764;  // 40 ms at 44.1 kHz
const LIMIT = 6615;  // 150 ms

class Fm1Output extends AudioWorkletProcessor {
  constructor() {
    super();
    this.capacity = 44100; // one second of stereo frames
    this.buffer = new Float32Array(this.capacity * 2);
    this.read = 0;
    this.size = 0;
    this.playing = false;
    const receive = ({ data }) => {
      if (data === "clear") { this.read = 0; this.size = 0; this.playing = false; return; }
      if (data && data.port) { data.port.onmessage = receive; return; }
      this.push(data);
    };
    this.port.onmessage = receive;
  }

  push(samples) {
    const frames = samples.length / 2;
    for (let i = 0; i < frames; i++) {
      if (this.size === this.capacity) { // drop oldest when full
        this.read = (this.read + 1) % this.capacity;
        this.size--;
      }
      const w = (this.read + this.size) % this.capacity;
      this.buffer[w * 2] = samples[i * 2];
      this.buffer[w * 2 + 1] = samples[i * 2 + 1];
      this.size++;
    }
    if (this.size > LIMIT) {
      const drop = this.size - START;
      this.read = (this.read + drop) % this.capacity;
      this.size -= drop;
    }
    if (this.size >= START) this.playing = true;
  }

  process(_inputs, outputs) {
    const [left, right] = outputs[0];
    for (let i = 0; i < left.length; i++) {
      if (this.playing && this.size) {
        left[i] = this.buffer[this.read * 2];
        right[i] = this.buffer[this.read * 2 + 1];
        this.read = (this.read + 1) % this.capacity;
        this.size--;
      } else {
        left[i] = right[i] = 0;
        this.playing = false;
      }
    }
    return true;
  }
}
registerProcessor("fm1-output", Fm1Output);
