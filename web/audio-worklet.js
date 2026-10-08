// SPDX-License-Identifier: GPL-3.0-only
// Plays interleaved stereo frames posted from the main thread. When the
// emulator runs slower than real time the buffer underruns into silence.
class Fm1Output extends AudioWorkletProcessor {
  constructor() {
    super();
    this.capacity = 44100 * 2; // one second of stereo
    this.buffer = new Float32Array(this.capacity * 2);
    this.read = 0;
    this.size = 0;
    this.port.onmessage = ({ data }) => {
      if (data === "clear") { this.read = 0; this.size = 0; return; }
      const samples = data;
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
    };
  }
  process(_inputs, outputs) {
    const [left, right] = outputs[0];
    for (let i = 0; i < left.length; i++) {
      if (this.size) {
        left[i] = this.buffer[this.read * 2];
        right[i] = this.buffer[this.read * 2 + 1];
        this.read = (this.read + 1) % this.capacity;
        this.size--;
      } else {
        left[i] = right[i] = 0;
      }
    }
    return true;
  }
}
registerProcessor("fm1-output", Fm1Output);
