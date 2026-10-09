// SPDX-License-Identifier: GPL-3.0-only
// Instantiate the emulator module with its block compiler. Shared by the
// browser worker and the Node.js tools.

/** Instantiate fm1.wasm from its bytes; returns the module's exports. */
export async function instantiateFm1(bytes) {
  let exports = null;
  const env = {
    // Compile one translated block (a module importing env.memory and
    // env.exec, exporting b: () -> i32) and append b to the function table.
    jit_compile(pointer, length) {
      try {
        const code = new Uint8Array(exports.memory.buffer, pointer, length);
        const module = new WebAssembly.Module(code);
        const instance = new WebAssembly.Instance(module, {
          env: { memory: exports.memory, exec: exports.fm1_jit_exec, exec_pred: exports.fm1_jit_exec_pred },
        });
        const table = exports.__indirect_function_table;
        const index = table.grow(1);
        table.set(index, instance.exports.b);
        return index;
      } catch (error) {
        console.error("block compilation failed", error);
        return -1;
      }
    },
  };
  const { instance } = await WebAssembly.instantiate(bytes, { env });
  exports = instance.exports;
  return exports;
}
