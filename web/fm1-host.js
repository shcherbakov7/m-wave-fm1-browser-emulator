// SPDX-License-Identifier: GPL-3.0-only
// Instantiate the emulator module with its block compiler. Shared by the
// browser worker and the Node.js tools.

// (module (type (func (param i32) (result i32)))
//   (table 1 funcref)
//   (func (type 0) (local.get 0) (i32.const 0) (return_call_indirect (type 0))))
const TAIL_CALL_PROBE = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
  0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f,
  0x03, 0x02, 0x01, 0x00,
  0x04, 0x04, 0x01, 0x70, 0x00, 0x01,
  0x0a, 0x0b, 0x01, 0x09, 0x00, 0x20, 0x00, 0x41, 0x00, 0x13, 0x00, 0x00, 0x0b,
]);

/** Whether this engine supports WebAssembly tail calls. */
export function supportsTailCalls() {
  try { return WebAssembly.validate(TAIL_CALL_PROBE); } catch { return false; }
}

/**
 * Instantiate fm1.wasm from its bytes; returns the module's exports.
 * Translated blocks call each other directly (chaining) when the engine
 * supports tail calls, unless `options.chaining` is false.
 */
export async function instantiateFm1(bytes, options = {}) {
  let exports = null;
  let free = 0, end = 0; // never used function table slots [free, end)
  const released = [];   // slots of functions the emulator dropped
  const env = {
    // Compile one translated block (a module importing env.memory, env.exec,
    // env.exec_pred and, when chaining, env.table; exporting b: (i32) -> i32)
    // and store b in the function table; returns its index.
    jit_compile(pointer, length) {
      try {
        const code = new Uint8Array(exports.memory.buffer, pointer, length);
        const module = new WebAssembly.Module(code);
        const table = exports.__indirect_function_table;
        const instance = new WebAssembly.Instance(module, {
          env: { memory: exports.memory, exec: exports.fm1_jit_exec, exec_pred: exports.fm1_jit_exec_pred, table },
        });
        let index = released.pop();
        if (index === undefined) {
          // Growing copies the table, so grow it in chunks.
          if (free === end) { free = table.grow(256); end = free + 256; }
          index = free++;
        }
        table.set(index, instance.exports.b);
        return index;
      } catch (error) {
        console.error("block compilation failed", error);
        return -1;
      }
    },
    // A translated function is no longer used: drop it and reuse its slot.
    jit_release(index) {
      exports.__indirect_function_table.set(index, null);
      released.push(index);
    },
  };
  const { instance } = await WebAssembly.instantiate(bytes, { env });
  exports = instance.exports;
  if (options.chaining !== false && supportsTailCalls()) exports.fm1_set_jit_chaining(1);
  return exports;
}
