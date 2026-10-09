// SPDX-License-Identifier: GPL-3.0-only
// Minimal WebAssembly binary encoder for translated guest blocks.

/// Function body bytes with helpers for the instructions the translator uses.
#[derive(Default)]
pub(crate) struct Body {
    pub(crate) bytes: Vec<u8>,
}

fn unsigned_leb(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn signed_leb(out: &mut Vec<u8>, mut value: i32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

// Opcodes (WebAssembly core 1.0 plus sign-extension operators).
pub(crate) const I32_EQZ: u8 = 0x45;
pub(crate) const I32_EQ: u8 = 0x46;
pub(crate) const I32_NE: u8 = 0x47;
pub(crate) const I32_LT_S: u8 = 0x48;
pub(crate) const I32_LT_U: u8 = 0x49;
pub(crate) const I32_GT_S: u8 = 0x4a;
pub(crate) const I32_GT_U: u8 = 0x4b;
pub(crate) const I32_LE_S: u8 = 0x4c;
pub(crate) const I32_LE_U: u8 = 0x4d;
pub(crate) const I32_GE_S: u8 = 0x4e;
pub(crate) const I32_GE_U: u8 = 0x4f;
pub(crate) const I32_ADD: u8 = 0x6a;
pub(crate) const I32_SUB: u8 = 0x6b;
pub(crate) const I32_MUL: u8 = 0x6c;
pub(crate) const I32_AND: u8 = 0x71;
pub(crate) const I32_OR: u8 = 0x72;
pub(crate) const I32_XOR: u8 = 0x73;
pub(crate) const I32_SHL: u8 = 0x74;
pub(crate) const I32_SHR_S: u8 = 0x75;
pub(crate) const I32_SHR_U: u8 = 0x76;
pub(crate) const I32_ROTR: u8 = 0x78;
pub(crate) const I32_EXTEND8_S: u8 = 0xc0;
pub(crate) const I32_EXTEND16_S: u8 = 0xc1;
pub(crate) const I32_CLZ: u8 = 0x67;
pub(crate) const I32_ROTL: u8 = 0x77;
pub(crate) const SELECT: u8 = 0x1b;
pub(crate) const DROP: u8 = 0x1a;

impl Body {
    pub(crate) fn op(&mut self, opcode: u8) -> &mut Self {
        self.bytes.push(opcode);
        self
    }
    pub(crate) fn i32(&mut self, value: i32) -> &mut Self {
        self.bytes.push(0x41);
        signed_leb(&mut self.bytes, value);
        self
    }
    pub(crate) fn u32(&mut self, value: u32) -> &mut Self {
        self.i32(value as i32)
    }
    pub(crate) fn get(&mut self, local: u32) -> &mut Self {
        self.bytes.push(0x20);
        unsigned_leb(&mut self.bytes, local);
        self
    }
    pub(crate) fn set(&mut self, local: u32) -> &mut Self {
        self.bytes.push(0x21);
        unsigned_leb(&mut self.bytes, local);
        self
    }
    fn memory(&mut self, opcode: u8, align: u32, offset: u32) -> &mut Self {
        self.bytes.push(opcode);
        unsigned_leb(&mut self.bytes, align);
        unsigned_leb(&mut self.bytes, offset);
        self
    }
    /// Load of `size` bytes (1, 2 or 4), zero- or sign-extended.
    pub(crate) fn load(&mut self, size: u8, sign: bool, offset: u32) -> &mut Self {
        match (size, sign) {
            (4, _) => self.memory(0x28, 2, offset),
            (2, false) => self.memory(0x2f, 1, offset),
            (2, true) => self.memory(0x2e, 1, offset),
            (1, false) => self.memory(0x2d, 0, offset),
            _ => self.memory(0x2c, 0, offset),
        }
    }
    pub(crate) fn store(&mut self, size: u8, offset: u32) -> &mut Self {
        match size {
            4 => self.memory(0x36, 2, offset),
            2 => self.memory(0x3b, 1, offset),
            _ => self.memory(0x3a, 0, offset),
        }
    }
    pub(crate) fn if_(&mut self) -> &mut Self {
        self.bytes.extend_from_slice(&[0x04, 0x40]);
        self
    }
    pub(crate) fn else_(&mut self) -> &mut Self {
        self.bytes.push(0x05);
        self
    }
    pub(crate) fn end(&mut self) -> &mut Self {
        self.bytes.push(0x0b);
        self
    }
    pub(crate) fn br(&mut self, depth: u32) -> &mut Self {
        self.bytes.push(0x0c);
        unsigned_leb(&mut self.bytes, depth);
        self
    }
    /// `br_table` over `targets` with `default`.
    pub(crate) fn br_table(&mut self, targets: &[u32], default: u32) -> &mut Self {
        self.bytes.push(0x0e);
        unsigned_leb(&mut self.bytes, targets.len() as u32);
        for &target in targets {
            unsigned_leb(&mut self.bytes, target);
        }
        unsigned_leb(&mut self.bytes, default);
        self
    }
    /// `block` / `loop` with no result.
    pub(crate) fn block(&mut self) -> &mut Self {
        self.bytes.extend_from_slice(&[0x02, 0x40]);
        self
    }
    pub(crate) fn loop_(&mut self) -> &mut Self {
        self.bytes.extend_from_slice(&[0x03, 0x40]);
        self
    }
    pub(crate) fn unreachable(&mut self) -> &mut Self {
        self.bytes.push(0x00);
        self
    }
    pub(crate) fn ret(&mut self) -> &mut Self {
        self.bytes.push(0x0f);
        self
    }
    pub(crate) fn call(&mut self, function: u32) -> &mut Self {
        self.bytes.push(0x10);
        unsigned_leb(&mut self.bytes, function);
        self
    }
}

fn section(out: &mut Vec<u8>, id: u8, content: &[u8]) {
    out.push(id);
    unsigned_leb(out, content.len() as u32);
    out.extend_from_slice(content);
}

fn name(out: &mut Vec<u8>, text: &str) {
    unsigned_leb(out, text.len() as u32);
    out.extend_from_slice(text.as_bytes());
}

/// A module importing `env.memory`, `env.exec: () -> i32` (function 0) and
/// `env.exec_pred: (i32, i32) -> i32` (function 1), exporting one function
/// `b: () -> i32` with `locals` i32 locals and the given body.
pub(crate) fn module(body: &Body, locals: u32) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    // Type 0: () -> i32; type 1: (i32, i32) -> i32.
    section(&mut out, 1, &[2, 0x60, 0, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7f]);
    let mut imports = vec![3];
    name(&mut imports, "env");
    name(&mut imports, "memory");
    imports.extend_from_slice(&[0x02, 0x00, 0x01]); // memory, min 1 page
    name(&mut imports, "env");
    name(&mut imports, "exec");
    imports.extend_from_slice(&[0x00, 0x00]);
    name(&mut imports, "env");
    name(&mut imports, "exec_pred");
    imports.extend_from_slice(&[0x00, 0x01]);
    section(&mut out, 2, &imports);
    section(&mut out, 3, &[1, 0]);
    let mut exports = vec![1];
    name(&mut exports, "b");
    exports.extend_from_slice(&[0x00, 0x02]); // function 2, after the imports
    section(&mut out, 7, &exports);
    let mut function = Vec::new();
    if locals == 0 {
        function.push(0);
    } else {
        function.push(1);
        unsigned_leb(&mut function, locals);
        function.push(0x7f);
    }
    function.extend_from_slice(&body.bytes);
    function.push(0x0b);
    let mut code = vec![1];
    unsigned_leb(&mut code, function.len() as u32);
    code.extend_from_slice(&function);
    section(&mut out, 10, &code);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn leb128_round_trips_boundaries() {
        for value in [0, 63, 64, -64, -65, 127, 128, i32::MAX, i32::MIN, -1] {
            let mut out = Vec::new();
            signed_leb(&mut out, value);
            let (mut result, mut shift) = (0i64, 0);
            for byte in &out {
                result |= ((byte & 0x7f) as i64) << shift;
                shift += 7;
            }
            if shift < 64 && out.last().unwrap() & 0x40 != 0 {
                result |= -1i64 << shift;
            }
            assert_eq!(result as i32, value);
        }
    }
}
