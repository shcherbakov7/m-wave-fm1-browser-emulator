// SPDX-License-Identifier: GPL-3.0-only
//! Translation of pi32v2 code into WebAssembly functions.
//!
//! A translated block is a function `() -> i32` in its own module. It works
//! directly on the CPU state and SRAM in the host's linear memory, at the
//! addresses given by [`Layout`], and returns how many guest instructions it
//! executed. On every return the PC word holds the next guest PC.
//!
//! Register, branch and SRAM instructions are translated; anything else
//! (device registers, flash data, faults, rare or state-changing forms) calls
//! the imported `exec`, which runs the instruction in the interpreter, and the
//! block ends after it. Semantics mirror `Cpu::execute` and
//! `extended::execute`; `tests` compares both on generated instructions.
mod encode;
#[cfg(test)]
mod tests;

use crate::{
    bus::Bus,
    cpu::signed,
    decode::{Cache as Decode, Extended, First, Wide},
    extended::packed,
    RAM, RAM_SIZE, XIP, XIP_END,
};
use encode::*;

/// Where the translated code finds guest state in linear memory.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// `r[0..16]` as consecutive u32 words.
    pub r: u32,
    /// `sr[0..16]` as consecutive u32 words.
    pub sr: u32,
    /// The guest PC word.
    pub pc: u32,
    /// Byte 0 of guest SRAM (guest address `RAM`).
    pub ram: u32,
    /// Nonzero while a CPU write-protection window is enabled.
    pub guards: u32,
    /// Bus write counter, incremented by translated SRAM stores.
    pub writes: u32,
}

pub struct Block {
    pub start: u32,
    /// Guest instructions in the longest path through the block.
    pub instructions: u32,
    pub wasm: Vec<u8>,
}

/// Longest straight-line run translated into one block.
const MAX_INSTRUCTIONS: u32 = 48;

// Locals.
const ADDR: u32 = 0;
const VALUE: u32 = 1;
const LEFT: u32 = 2;
const RIGHT: u32 = 3;
const RESULT: u32 = 4;
const WRITEBACK: u32 = 5;
const LOCALS: u32 = 6;

/// The imported interpreter step.
const EXEC: u32 = 0;

#[derive(Clone, Copy)]
enum Base {
    R(usize),
    Sp,
}

#[derive(Clone, Copy)]
enum Writeback {
    None,
    /// Base becomes the access address (pre-index).
    Address,
    /// Base becomes address + constant (post-increment).
    Post(i32),
    /// Base becomes address + r[n] (post-increment by register).
    PostRegister(usize),
}

#[derive(Clone, Copy)]
struct Mem {
    reg: usize,
    base: Base,
    offset: i32,
    index: Option<(usize, u32)>,
    size: u8,
    store: bool,
    sign: bool,
    writeback: Writeback,
}

#[derive(Clone, Copy, PartialEq)]
enum Flow {
    Continue,
    End,
}

#[derive(Clone, Copy)]
enum Cond {
    Eq,
    Ne,
    GeU,
    LtU,
    GtU,
    LeU,
    GeS,
    LtS,
    GtS,
    LeS,
}

impl Cond {
    fn opcode(self) -> u8 {
        match self {
            Cond::Eq => I32_EQ,
            Cond::Ne => I32_NE,
            Cond::GeU => I32_GE_U,
            Cond::LtU => I32_LT_U,
            Cond::GtU => I32_GT_U,
            Cond::LeU => I32_LE_U,
            Cond::GeS => I32_GE_S,
            Cond::LtS => I32_LT_S,
            Cond::GtS => I32_GT_S,
            Cond::LeS => I32_LE_S,
        }
    }
}

struct Translator<'a> {
    body: Body,
    layout: &'a Layout,
    /// Instructions emitted before the current one.
    count: u32,
}

impl Translator<'_> {
    fn r(&mut self, n: usize) {
        self.body.i32(0).load(4, false, self.layout.r + 4 * n as u32);
    }
    fn sr(&mut self, n: usize) {
        self.body.i32(0).load(4, false, self.layout.sr + 4 * n as u32);
    }
    /// `r[n] = local`.
    fn set_r(&mut self, n: usize, local: u32) {
        self.body.i32(0).get(local).store(4, self.layout.r + 4 * n as u32);
    }
    fn set_sr(&mut self, n: usize, local: u32) {
        self.body.i32(0).get(local).store(4, self.layout.sr + 4 * n as u32);
    }
    /// `r[n] = constant`.
    fn set_r_const(&mut self, n: usize, value: u32) {
        self.body.i32(0).u32(value).store(4, self.layout.r + 4 * n as u32);
    }
    fn set_pc_const(&mut self, pc: u32) {
        self.body.i32(0).u32(pc).store(4, self.layout.pc);
    }
    fn set_pc_local(&mut self, local: u32) {
        self.body.i32(0).get(local).store(4, self.layout.pc);
    }
    fn base(&mut self, base: Base) {
        match base {
            Base::R(n) => self.r(n),
            Base::Sp => self.sr(14),
        }
    }
    fn set_base(&mut self, base: Base, local: u32) {
        match base {
            Base::R(n) => self.set_r(n, local),
            Base::Sp => self.set_sr(14, local),
        }
    }
    /// Return after the current instruction, which ran to completion.
    fn finish(&mut self) {
        self.body.u32(self.count + 1).ret();
    }
    /// Run the current instruction in the interpreter and end the block.
    fn exec(&mut self, pc: u32) {
        self.set_pc_const(pc);
        self.body.call(EXEC).op(DROP);
        self.finish();
    }
    /// Pushes 1 when `[ADDR + low, ADDR + high)` is aligned SRAM that the
    /// translated code may access directly (`size` gives the alignment).
    fn fast(&mut self, low: i32, high: i32, size: u8, store: bool) {
        let span = (high - low) as u32;
        self.body
            .get(ADDR)
            .i32(low)
            .op(I32_ADD)
            .u32(RAM)
            .op(I32_SUB)
            .u32(RAM_SIZE as u32 - span)
            .op(I32_LE_U);
        if size > 1 {
            self.body
                .get(ADDR)
                .u32(size as u32 - 1)
                .op(I32_AND)
                .op(I32_EQZ)
                .op(I32_AND);
        }
        if store {
            self.body
                .i32(0)
                .load(4, false, self.layout.guards)
                .op(I32_EQZ)
                .op(I32_AND);
        }
    }
    /// Push the SRAM byte offset of guest address `ADDR + offset`.
    fn ram_offset(&mut self, offset: i32) {
        self.body.get(ADDR).i32(offset).op(I32_ADD).u32(RAM).op(I32_SUB);
    }
    fn count_write(&mut self) {
        let writes = self.layout.writes;
        self.body
            .i32(0)
            .i32(0)
            .load(4, false, writes)
            .i32(1)
            .op(I32_ADD)
            .store(4, writes);
    }
    /// Store local `value` at `ADDR + offset` (already checked).
    fn store_at(&mut self, offset: i32, size: u8, value: u32) {
        self.ram_offset(offset);
        self.body.get(value).store(size, self.layout.ram);
        self.count_write();
    }
    /// Load from `ADDR + offset` (already checked) into local `into`.
    fn load_at(&mut self, offset: i32, size: u8, sign: bool, into: u32) {
        self.ram_offset(offset);
        self.body.load(size, sign, self.layout.ram).set(into);
    }

    fn mem(&mut self, pc: u32, m: Mem) {
        self.base(m.base);
        if m.offset != 0 {
            self.body.i32(m.offset).op(I32_ADD);
        }
        if let Some((index, shift)) = m.index {
            self.r(index);
            if shift != 0 {
                self.body.u32(shift).op(I32_SHL);
            }
            self.body.op(I32_ADD);
        }
        self.body.set(ADDR);
        // The new base value is computed from the incoming registers.
        match m.writeback {
            Writeback::None => {}
            Writeback::Address => {
                self.body.get(ADDR).set(WRITEBACK);
            }
            Writeback::Post(delta) => {
                self.body.get(ADDR).i32(delta).op(I32_ADD).set(WRITEBACK);
            }
            Writeback::PostRegister(n) => {
                self.body.get(ADDR);
                self.r(n);
                self.body.op(I32_ADD).set(WRITEBACK);
            }
        }
        self.fast(0, m.size as i32, m.size, m.store);
        self.body.if_();
        if m.store {
            self.r(m.reg);
            self.body.set(VALUE);
            self.store_at(0, m.size, VALUE);
        } else {
            self.load_at(0, m.size, m.sign, VALUE);
            self.set_r(m.reg, VALUE);
        }
        if !matches!(m.writeback, Writeback::None) {
            self.set_base(m.base, WRITEBACK);
        }
        self.body.else_();
        self.exec(pc);
        self.body.end();
    }

    /// Push the values of `sources` (in order) onto the stack: each push
    /// lowers SP by 4 and stores there.
    fn push(&mut self, pc: u32, sources: &[Source]) {
        let words = sources.len() as i32;
        self.sr(14);
        self.body.set(ADDR);
        self.fast(-4 * words, 0, 4, true);
        self.body.if_();
        for (i, source) in sources.iter().enumerate() {
            match *source {
                Source::R(n) => self.r(n),
                Source::Sr(n) => self.sr(n),
            }
            self.body.set(VALUE);
            self.store_at(-4 * (i as i32 + 1), 4, VALUE);
        }
        self.body.get(ADDR).i32(-4 * words).op(I32_ADD).set(VALUE);
        self.set_sr(14, VALUE);
        self.body.else_();
        self.exec(pc);
        self.body.end();
    }

    /// Pop into `targets` in order; `Target::Pc` ends the block there.
    fn pop(&mut self, pc: u32, targets: &[Target]) -> Flow {
        let words = targets.len() as i32;
        self.sr(14);
        self.body.set(ADDR);
        self.fast(0, 4 * words, 4, false);
        self.body.if_();
        let mut flow = Flow::Continue;
        for (i, target) in targets.iter().enumerate() {
            self.load_at(4 * i as i32, 4, false, VALUE);
            match *target {
                Target::R(n) => self.set_r(n, VALUE),
                Target::Sr(n) => self.set_sr(n, VALUE),
                Target::Pc => {
                    self.set_pc_local(VALUE);
                    flow = Flow::End;
                }
            }
        }
        self.body.get(ADDR).i32(4 * words).op(I32_ADD).set(VALUE);
        self.set_sr(14, VALUE);
        if flow == Flow::End {
            self.finish();
        }
        self.body.else_();
        self.exec(pc);
        self.body.end();
        flow
    }

    /// `dest = left ± right`, setting PSR V/C/Z/N as `Cpu::arithmetic` does
    /// (no carry in). LEFT and RIGHT must hold the operands.
    fn arithmetic(&mut self, dest: usize, subtract: bool) {
        self.body
            .get(LEFT)
            .get(RIGHT)
            .op(if subtract { I32_SUB } else { I32_ADD })
            .set(RESULT);
        // VALUE = PSR with NZCV recomputed.
        self.sr(5);
        self.body.i32(!15).op(I32_AND);
        // V: signed overflow.
        if subtract {
            self.body
                .get(LEFT)
                .get(RIGHT)
                .op(I32_XOR)
                .get(LEFT)
                .get(RESULT)
                .op(I32_XOR)
                .op(I32_AND);
        } else {
            self.body
                .get(LEFT)
                .get(RESULT)
                .op(I32_XOR)
                .get(RIGHT)
                .get(RESULT)
                .op(I32_XOR)
                .op(I32_AND);
        }
        self.body.i32(31).op(I32_SHR_U).op(I32_OR);
        // C: carry out (add) / no borrow (subtract).
        if subtract {
            self.body.get(LEFT).get(RIGHT).op(I32_GE_U);
        } else {
            self.body.get(RESULT).get(LEFT).op(I32_LT_U);
        }
        self.body.i32(1).op(I32_SHL).op(I32_OR);
        // Z, N.
        self.body
            .get(RESULT)
            .op(I32_EQZ)
            .i32(2)
            .op(I32_SHL)
            .op(I32_OR)
            .get(RESULT)
            .i32(31)
            .op(I32_SHR_U)
            .i32(3)
            .op(I32_SHL)
            .op(I32_OR)
            .set(VALUE);
        self.set_sr(5, VALUE);
        self.set_r(dest, RESULT);
    }
}

#[derive(Clone, Copy)]
enum Source {
    R(usize),
    Sr(usize),
}

#[derive(Clone, Copy)]
enum Target {
    R(usize),
    Sr(usize),
    Pc,
}

/// Translate the block starting at `start`, or `None` if `start` is not
/// translatable application code.
pub(crate) fn translate(bus: &Bus, decode: &mut Decode, layout: &Layout, start: u32) -> Option<Block> {
    translate_limited(bus, decode, layout, start, MAX_INSTRUCTIONS)
}

pub(crate) fn translate_limited(
    bus: &Bus,
    decode: &mut Decode,
    layout: &Layout,
    start: u32,
    limit: u32,
) -> Option<Block> {
    if !(XIP..XIP_END).contains(&start) {
        return None;
    }
    let mut t = Translator {
        body: Body::default(),
        layout,
        count: 0,
    };
    let mut pc = start;
    loop {
        let h = bus.fetch(pc).ok()? as u32;
        let flow = instruction(&mut t, bus, decode, pc, h);
        t.count += 1;
        let Some((flow, length)) = flow else {
            break;
        };
        pc = pc.wrapping_add(length);
        if flow == Flow::End {
            break;
        }
        if t.count >= limit || !(XIP..XIP_END).contains(&pc) {
            t.set_pc_const(pc);
            t.body.u32(t.count).ret();
            break;
        }
    }
    // Every path returns explicitly; this value is never used.
    t.body.u32(t.count);
    Some(Block {
        start,
        instructions: t.count,
        wasm: module(&t.body, LOCALS),
    })
}

/// Emit one instruction. `None` means it was handed to the interpreter and
/// the block ended; otherwise the flow and the instruction length in bytes.
fn instruction(
    t: &mut Translator<'_>,
    bus: &Bus,
    decode: &mut Decode,
    pc: u32,
    h: u32,
) -> Option<(Flow, u32)> {
    let word = |offset: u32| -> Option<u32> {
        let address = pc.wrapping_add(offset);
        (XIP..XIP_END)
            .contains(&address)
            .then(|| bus.fetch(address).ok().map(u32::from))
            .flatten()
    };
    // Parallel bundles keep their simultaneous-source semantics in the
    // interpreter.
    if h >> 13 == 6 || h & 0xf800 == 0xf000 {
        t.exec(pc);
        return None;
    }
    let a = (h & 7) as usize;
    let b = ((h >> 4) & 7) as usize;
    let n = (h & 15) as usize;
    let next = pc.wrapping_add(2);
    match decode.first(h) {
        First::MoveImmediate32 => {
            let value = word(2)? | (word(4)? << 16);
            if h & 0xfff0 == 0xffc0 {
                t.set_r_const(n, value);
            } else if matches!(n, 0 | 12 | 13 | 14) {
                t.body.u32(value).set(VALUE);
                t.set_sr(n, VALUE);
            } else {
                t.exec(pc);
                return None;
            }
            Some((Flow::Continue, 6))
        }
        First::MovMask => {
            let extra = word(2)?;
            if (extra >> 10) & 3 == 0 && extra & 0x0f00 > 0x0300 {
                t.exec(pc);
                return None;
            }
            t.set_r_const(((extra >> 12) & 15) as usize, packed(extra));
            Some((Flow::Continue, 4))
        }
        First::MovImm16 => {
            t.set_r_const(n, signed(word(2)?, 16) as u32);
            Some((Flow::Continue, 4))
        }
        First::MovImm8 => {
            t.set_r_const(a, (((h >> 3) & 7) << 5) | ((h >> 8) & 31));
            Some((Flow::Continue, 2))
        }
        First::MovNegative => {
            t.set_r_const(a, 0xffff_ffe0 | ((h >> 8) & 31));
            Some((Flow::Continue, 2))
        }
        First::MovReg => {
            t.r(((h >> 4) & 15) as usize);
            t.body.set(VALUE);
            t.set_r(n, VALUE);
            Some((Flow::Continue, 2))
        }
        First::Arithmetic => {
            let c = (((h >> 7) & 3) * 2 + ((h >> 3) & 1)) as usize;
            t.r(b);
            t.body.set(LEFT);
            t.r(c);
            t.body.set(RIGHT);
            t.arithmetic(a, h & 0xfe00 == 0x1e00);
            Some((Flow::Continue, 2))
        }
        First::AddImm8 => {
            let imm = signed((((h >> 3) & 7) << 5) | ((h >> 8) & 31), 8);
            t.r(a);
            t.body.set(LEFT).i32(imm).set(RIGHT);
            t.arithmetic(a, false);
            Some((Flow::Continue, 2))
        }
        First::AddSp => {
            let imm = (signed((h >> 5) & 7, 3) << 7) | (((h >> 8) & 31) << 2) as i32;
            t.sr(14);
            t.body.i32(imm).op(I32_ADD).set(VALUE);
            t.set_sr(14, VALUE);
            Some((Flow::Continue, 2))
        }
        First::AddSmall => {
            t.r(b);
            t.body.set(LEFT).u32((h >> 8) & 31).set(RIGHT);
            t.arithmetic(a, false);
            Some((Flow::Continue, 2))
        }
        First::Logic => {
            match h & 0xff88 {
                0x1900 => {
                    t.r(a);
                    t.r(b);
                    t.body.op(I32_OR);
                }
                0x1908 => {
                    t.r(a);
                    t.r(b);
                    t.body.op(I32_XOR);
                }
                0x1980 => {
                    t.r(a);
                    t.r(b);
                    t.body.op(I32_AND);
                }
                _ => {
                    t.r(b);
                    t.body.i32(-1).op(I32_XOR);
                }
            }
            t.body.set(VALUE);
            t.set_r(a, VALUE);
            Some((Flow::Continue, 2))
        }
        First::Asr => {
            t.r(b);
            t.body.u32((h >> 8) & 31).op(I32_SHR_S).set(VALUE);
            t.set_r(a, VALUE);
            Some((Flow::Continue, 2))
        }
        First::ShiftImmediate => {
            t.r(b);
            t.body
                .u32((h >> 8) & 31)
                .op(if h & 0x80 != 0 { I32_SHR_U } else { I32_SHL })
                .set(VALUE);
            t.set_r(a, VALUE);
            Some((Flow::Continue, 2))
        }
        First::MemoryWord => {
            t.mem(
                pc,
                Mem {
                    reg: a,
                    base: Base::R(b),
                    offset: signed((h >> 8) & 31, 5) * 4,
                    index: None,
                    size: 4,
                    store: h & 0x80 != 0,
                    sign: false,
                    writeback: Writeback::None,
                },
            );
            Some((Flow::Continue, 2))
        }
        First::PushRegs => {
            let boundary = (h & 15) as usize;
            let range: Vec<_> = if boundary < 4 {
                (boundary..=3).rev().map(Source::R).collect()
            } else {
                (4..=boundary).rev().map(Source::R).collect()
            };
            t.push(pc, &range);
            Some((Flow::Continue, 2))
        }
        First::PopRegs => {
            let boundary = (h & 15) as usize;
            let range: Vec<_> = if boundary < 4 {
                (boundary..=3).map(Target::R).collect()
            } else {
                (4..=boundary).map(Target::R).collect()
            };
            t.pop(pc, &range);
            Some((Flow::Continue, 2))
        }
        First::PushRets => {
            t.push(pc, &[Source::Sr(3)]);
            Some((Flow::Continue, 2))
        }
        First::PopPc => {
            t.pop(pc, &[Target::Pc]);
            Some((Flow::End, 2))
        }
        First::PushRetsRegs => {
            let mut sources = vec![Source::Sr(3)];
            sources.extend((4..=(h & 15) as usize).rev().map(Source::R));
            t.push(pc, &sources);
            Some((Flow::Continue, 2))
        }
        First::PopRetsRegs => {
            let mut targets: Vec<_> = (4..=(h & 15) as usize).map(Target::R).collect();
            targets.push(Target::Sr(3));
            t.pop(pc, &targets);
            Some((Flow::Continue, 2))
        }
        First::PopPcRegs => {
            let mut targets: Vec<_> = (4..=(h & 15) as usize).map(Target::R).collect();
            targets.push(Target::Pc);
            t.pop(pc, &targets);
            Some((Flow::End, 2))
        }
        First::MoveStackPointer => {
            let (from, to) = match h {
                0x1440 => (12, 14),
                0x1441 => (13, 14),
                0x1442 => (14, 12),
                _ => (14, 13),
            };
            t.sr(from);
            t.body.set(VALUE);
            t.set_sr(to, VALUE);
            Some((Flow::Continue, 2))
        }
        First::CallRel32 => {
            let displacement = word(2)? | (word(4)? << 16);
            t.body.u32(pc + 6).set(VALUE);
            t.set_sr(3, VALUE);
            t.set_pc_const((pc + 6).wrapping_add(displacement));
            t.finish();
            Some((Flow::End, 6))
        }
        First::Relative22 => {
            let displacement = signed(((h & 63) << 16) | word(2)?, 22) * 2;
            if h & 0xffc0 == 0xea80 {
                t.body.u32(pc + 4).set(VALUE);
                t.set_sr(3, VALUE);
            }
            t.set_pc_const((pc + 4).wrapping_add(displacement as u32));
            t.finish();
            Some((Flow::End, 4))
        }
        First::CallRel9 => {
            let displacement = signed((((h >> 4) & 7) << 6) | (((h >> 8) & 31) << 1), 9);
            t.body.u32(next).set(VALUE);
            t.set_sr(3, VALUE);
            t.set_pc_const(next.wrapping_add(displacement as u32));
            t.finish();
            Some((Flow::End, 2))
        }
        First::GotoRel12 => {
            let displacement = signed(
                ((h & 3) << 10) | (((h >> 4) & 15) << 6) | (((h >> 8) & 31) << 1),
                12,
            );
            t.set_pc_const(next.wrapping_add(displacement as u32));
            t.finish();
            Some((Flow::End, 2))
        }
        First::BranchZero => {
            let displacement = signed((((h >> 4) & 7) << 6) | (((h >> 8) & 31) << 1), 9);
            t.r(a);
            t.body.set(LEFT).i32(0).set(RIGHT);
            let cond = if h & 0x80 != 0 { Cond::Ne } else { Cond::Eq };
            branch(t, cond, next.wrapping_add(displacement as u32), next);
            Some((Flow::End, 2))
        }
        First::Return => {
            t.sr(3);
            t.body.set(VALUE);
            t.set_pc_local(VALUE);
            t.finish();
            Some((Flow::End, 2))
        }
        First::CallReg => {
            t.body.u32(next).set(VALUE);
            t.r(n);
            t.body.set(ADDR);
            t.set_sr(3, VALUE);
            t.set_pc_local(ADDR);
            t.finish();
            Some((Flow::End, 2))
        }
        First::SyncOrNop => Some((Flow::Continue, 2)),
        First::Extended(extended) => extended_instruction(t, bus, decode, pc, h, extended, word),
        _ => {
            t.exec(pc);
            None
        }
    }
}

/// Conditional branch on LEFT `cond` RIGHT.
fn branch(t: &mut Translator<'_>, cond: Cond, target: u32, fallthrough: u32) {
    t.body
        .u32(target)
        .u32(fallthrough)
        .get(LEFT)
        .get(RIGHT)
        .op(cond.opcode())
        .op(SELECT)
        .set(VALUE);
    t.set_pc_local(VALUE);
    t.finish();
}

fn extended_instruction(
    t: &mut Translator<'_>,
    _bus: &Bus,
    decode: &mut Decode,
    pc: u32,
    h: u32,
    kind: Extended,
    word: impl Fn(u32) -> Option<u32>,
) -> Option<(Flow, u32)> {
    let a = (h & 7) as usize;
    let b = ((h >> 4) & 7) as usize;
    let n = (h & 15) as usize;
    let m = ((h >> 4) & 15) as usize;
    match kind {
        Extended::MoveRegisterPair => {
            let destination = (h & 14) as usize;
            let source = ((h >> 4) & 14) as usize;
            t.r(source);
            t.body.set(LEFT);
            t.r(source + 1);
            t.body.set(RIGHT);
            t.set_r(destination, LEFT);
            t.set_r(destination + 1, RIGHT);
            Some((Flow::Continue, 2))
        }
        Extended::Multiply => {
            t.r(n);
            t.r(m);
            t.body.op(I32_MUL).set(VALUE);
            t.set_r(n, VALUE);
            Some((Flow::Continue, 2))
        }
        Extended::ClearPair => {
            let destination = (h & 14) as usize;
            t.set_r_const(destination, 0);
            t.set_r_const(destination + 1, 0);
            Some((Flow::Continue, 2))
        }
        Extended::AddRegister => {
            t.r(n);
            t.body.set(LEFT);
            t.r(m);
            t.body.set(RIGHT);
            t.arithmetic(n, false);
            Some((Flow::Continue, 2))
        }
        Extended::ClearHighRegister => {
            t.set_r_const(8 + a, 0);
            Some((Flow::Continue, 2))
        }
        Extended::CacheFlushInvalidate | Extended::Ssync => Some((Flow::Continue, 2)),
        Extended::StackWord => {
            t.mem(
                pc,
                Mem {
                    reg: a,
                    base: Base::Sp,
                    offset: ((((h >> 8) & 31) | (h & 32)) * 4) as i32,
                    index: None,
                    size: 4,
                    store: h & 128 != 0,
                    sign: false,
                    writeback: Writeback::None,
                },
            );
            Some((Flow::Continue, 2))
        }
        Extended::Extend => {
            t.r(b);
            match (h & 0x80 != 0, h & 8 != 0) {
                (false, true) => t.body.op(I32_EXTEND8_S),
                (true, true) => t.body.op(I32_EXTEND16_S),
                (false, false) => t.body.u32(0xff).op(I32_AND),
                (true, false) => t.body.u32(0xffff).op(I32_AND),
            };
            t.body.set(VALUE);
            t.set_r(a, VALUE);
            Some((Flow::Continue, 2))
        }
        Extended::MoveNegative => {
            t.set_r_const(a, (h >> 8) | 0xffff_ffe0);
            Some((Flow::Continue, 2))
        }
        Extended::BitRegister => {
            let bit = 1u32 << ((h >> 8) & 31);
            t.r(a);
            match h & 0xf8 {
                0x30 => t.body.u32(bit).op(I32_OR),
                0x38 => t.body.u32(bit).op(I32_XOR),
                _ => t.body.u32(!bit).op(I32_AND),
            };
            t.body.set(VALUE);
            t.set_r(a, VALUE);
            Some((Flow::Continue, 2))
        }
        Extended::ShiftRegister => {
            // Counts of 32 or more give 0, or all ones for a negative ASR.
            t.r(b);
            t.body.set(RIGHT);
            t.r(a);
            t.body.set(LEFT);
            let opcode = match h & 0x88 {
                0 => I32_SHL,
                0x80 => I32_SHR_U,
                _ => I32_SHR_S,
            };
            t.body.get(LEFT).get(RIGHT).op(opcode);
            if h & 0x88 == 0x88 {
                t.body.get(LEFT).i32(31).op(I32_SHR_S);
            } else {
                t.body.i32(0);
            }
            t.body
                .get(RIGHT)
                .u32(32)
                .op(I32_LT_U)
                .op(SELECT)
                .set(VALUE);
            t.set_r(a, VALUE);
            Some((Flow::Continue, 2))
        }
        Extended::AddStack => {
            t.sr(14);
            t.body
                .u32((((h >> 5) & 3) << 5) | ((h >> 8) & 31))
                .op(I32_ADD)
                .set(VALUE);
            t.set_r(a, VALUE);
            Some((Flow::Continue, 2))
        }
        Extended::GotoRegister => {
            t.r(n);
            t.body.set(VALUE);
            t.set_pc_local(VALUE);
            t.finish();
            Some((Flow::End, 2))
        }
        Extended::MemorySmall => {
            let size = if h & 0x2000 == 0 { 1 } else { 2 };
            t.mem(
                pc,
                Mem {
                    reg: a,
                    base: Base::R(b),
                    offset: signed((h >> 8) & 31, 5) * size as i32,
                    index: None,
                    size,
                    store: h & 0x80 != 0,
                    sign: false,
                    writeback: Writeback::None,
                },
            );
            Some((Flow::Continue, 2))
        }
        Extended::MemoryPostincrementRegister => {
            let store = h & 8 != 0;
            if !store && a == b {
                t.exec(pc);
                return None;
            }
            let size = match h & 0xfc00 {
                0x0800 => 4,
                0x0c00 => 2,
                _ => 1,
            };
            t.mem(
                pc,
                Mem {
                    reg: a,
                    base: Base::R(b),
                    offset: 0,
                    index: None,
                    size,
                    store,
                    sign: false,
                    writeback: Writeback::PostRegister(8 + ((h >> 7) & 7) as usize),
                },
            );
            Some((Flow::Continue, 2))
        }
        Extended::MemoryPostincrement => {
            let kind = (h >> 7) & 7;
            let size: u8 = match kind {
                2 | 3 => 4,
                4 | 5 => 2,
                _ => 1,
            };
            let delta = if h & 8 == 0 {
                size as i32
            } else {
                -(size as i32)
            };
            t.mem(
                pc,
                Mem {
                    reg: a,
                    base: Base::R(b),
                    offset: 0,
                    index: None,
                    size,
                    store: kind & 1 != 0,
                    sign: false,
                    writeback: Writeback::Post(delta),
                },
            );
            Some((Flow::Continue, 2))
        }
        Extended::Wide => {
            let x = word(2)?;
            wide_instruction(t, decode, pc, h, x, &word)
        }
        _ => {
            t.exec(pc);
            None
        }
    }
}

fn wide_instruction(
    t: &mut Translator<'_>,
    decode: &mut Decode,
    pc: u32,
    h: u32,
    x: u32,
    word: &impl Fn(u32) -> Option<u32>,
) -> Option<(Flow, u32)> {
    let n = (h & 15) as usize;
    let d = (x >> 12) as usize;
    let s = ((x >> 4) & 15) as usize;
    let c = ((x >> 8) & 15) as usize;
    let next = pc + 4;
    let mem = |t: &mut Translator<'_>, m: Mem| {
        t.mem(pc, m);
        Some((Flow::Continue, 4))
    };
    match decode.wide(h, x) {
        Wide::BranchCompareImmediate => {
            let kind = (h >> 7) & 63;
            let field = (((h >> 4) & 7) << 7) | (x >> 9);
            let (cond, value) = match kind {
                0x30 => (Cond::Eq, signed(field, 10) as u32),
                0x31 => (Cond::Ne, signed(field, 10) as u32),
                0x32 => (Cond::GeU, field),
                0x33 => (Cond::LtU, field),
                0x38 => (Cond::GtU, field),
                0x39 => (Cond::LeU, field),
                0x3a => (Cond::GeS, signed(field, 10) as u32),
                0x3b => (Cond::LtS, signed(field, 10) as u32),
                0x3c => (Cond::GtS, signed(field, 10) as u32),
                _ => (Cond::LeS, signed(field, 10) as u32),
            };
            t.r(n);
            t.body.set(LEFT).u32(value).set(RIGHT);
            branch(t, cond, next.wrapping_add((signed(x & 511, 9) * 2) as u32), next);
            Some((Flow::End, 4))
        }
        Wide::BranchCompareRegister => {
            let cond = match h & 0xfff0 {
                0xe800 => Cond::Eq,
                0xe880 => Cond::Ne,
                0xe900 => Cond::GeU,
                0xe980 => Cond::LtU,
                0xec00 => Cond::GtU,
                0xec80 => Cond::LeU,
                0xed00 => Cond::GeS,
                0xed80 => Cond::LtS,
                0xee00 => Cond::GtS,
                _ => Cond::LeS,
            };
            t.r(d);
            t.body.set(LEFT);
            t.r(n);
            t.body.set(RIGHT);
            branch(t, cond, next.wrapping_add((signed(x & 511, 9) * 2) as u32), next);
            Some((Flow::End, 4))
        }
        Wide::BranchRegisterMask => {
            t.r(n);
            t.r(((h >> 4) & 15) as usize);
            t.body.op(I32_AND).set(LEFT).i32(0).set(RIGHT);
            let cond = if h & 0x100 != 0 { Cond::Ne } else { Cond::Eq };
            branch(t, cond, next.wrapping_add((signed(x, 16) * 2) as u32), next);
            Some((Flow::End, 4))
        }
        Wide::DecrementBranch => {
            t.r(n);
            t.body.i32(1).op(I32_SUB).set(LEFT);
            t.set_r(n, LEFT);
            t.body.i32(0).set(RIGHT);
            branch(t, Cond::Ne, next.wrapping_add((signed(x, 16) * 2) as u32), next);
            Some((Flow::End, 4))
        }
        Wide::BranchLong => {
            let displacement = signed(word(4)?, 16) * 2;
            let after = pc + 6;
            let target = after.wrapping_add(displacement as u32);
            t.r(d);
            t.body.set(LEFT);
            match h & 0x60 {
                0 if matches!(h & 15, 0 | 1 | 10..=13) => {
                    t.body.i32(signed(x & 4095, 12));
                }
                0 => {
                    t.body.u32(x & 4095);
                }
                0x40 => t.r(c),
                _ => {
                    t.body.u32(packed(x));
                }
            }
            t.body.set(RIGHT);
            if h & 0x60 == 0x60 {
                t.body.get(LEFT).get(RIGHT).op(I32_AND).set(LEFT).i32(0).set(RIGHT);
                let cond = if h & 1 == 0 { Cond::Eq } else { Cond::Ne };
                branch(t, cond, target, after);
            } else {
                let cond = match h & 15 {
                    0 => Cond::Eq,
                    1 => Cond::Ne,
                    2 => Cond::GeU,
                    3 => Cond::LtU,
                    8 => Cond::GtU,
                    9 => Cond::LeU,
                    10 => Cond::GeS,
                    11 => Cond::LtS,
                    12 => Cond::GtS,
                    _ => Cond::LeS,
                };
                branch(t, cond, target, after);
            }
            Some((Flow::End, 6))
        }
        Wide::ByteExtended => {
            let offset = signed(((h & 1) << 8) | (((x >> 8) & 15) << 4) | (x & 15), 9);
            mem(
                t,
                Mem {
                    reg: d,
                    base: Base::R(s),
                    offset,
                    index: None,
                    size: 1,
                    store: h & 2 != 0,
                    sign: h & 4 != 0,
                    writeback: if h & 8 != 0 {
                        Writeback::Address
                    } else {
                        Writeback::None
                    },
                },
            )
        }
        Wide::WordExtended => {
            let offset =
                (signed(h & 7, 3) << 8) | (((x >> 8) & 15) << 4) as i32 | (x & 12) as i32;
            mem(
                t,
                Mem {
                    reg: d,
                    base: Base::R(s),
                    offset,
                    index: None,
                    size: 4,
                    store: x & 1 != 0,
                    sign: false,
                    writeback: if x & 2 != 0 {
                        Writeback::Address
                    } else {
                        Writeback::None
                    },
                },
            )
        }
        Wide::HalfwordExtended => {
            let store = x & 1 != 0;
            let high = if store {
                signed(h & 7, 3)
            } else {
                signed(h & 3, 2)
            };
            let offset = (high << 8) | (((x >> 8) & 15) << 4) as i32 | (x & 14) as i32;
            mem(
                t,
                Mem {
                    reg: d,
                    base: Base::R(s),
                    offset,
                    index: None,
                    size: 2,
                    store,
                    sign: !store && h & 4 != 0,
                    writeback: if h & 8 != 0 {
                        Writeback::Address
                    } else {
                        Writeback::None
                    },
                },
            )
        }
        Wide::MemoryIndexed => {
            let size: u8 = match h {
                0xecd8 => 4,
                0xedd8 => 2,
                _ => 1,
            };
            let mode = x & 7;
            let shift = if x & 8 != 0 { size.trailing_zeros() } else { 0 };
            mem(
                t,
                Mem {
                    reg: d,
                    base: Base::R(s),
                    offset: 0,
                    index: Some((c, shift)),
                    size,
                    store: if size == 4 { mode == 3 } else { mode == 1 },
                    sign: mode == 2 && size != 4,
                    writeback: Writeback::None,
                },
            )
        }
        Wide::BytePostincrementLoad | Wide::BytePostincrementStore => {
            let store = matches!(decode.wide(h, x), Wide::BytePostincrementStore);
            mem(
                t,
                Mem {
                    reg: d,
                    base: Base::R(s),
                    offset: 0,
                    index: None,
                    size: 1,
                    store,
                    sign: !store && h & 4 != 0,
                    writeback: Writeback::Post((((x >> 8) & 15) * 16 + (x & 15)) as i32),
                },
            )
        }
        Wide::HalfwordPostincrement => mem(
            t,
            Mem {
                reg: d,
                base: Base::R(s),
                offset: 0,
                index: None,
                size: 2,
                store: x & 1 != 0,
                sign: h & 4 != 0,
                writeback: Writeback::Post(signed(
                    ((h & 3) << 8) | (((x >> 8) & 15) << 4) | (x & 14),
                    10,
                )),
            },
        ),
        Wide::WordPostincrementLoad | Wide::WordPostincrementStore => mem(
            t,
            Mem {
                reg: d,
                base: Base::R(s),
                offset: 0,
                index: None,
                size: 4,
                store: x & 1 != 0,
                sign: false,
                writeback: Writeback::Post(
                    (signed(h & 7, 3) << 8) + ((((x >> 8) & 15) << 4) | (x & 12)) as i32,
                ),
            },
        ),
        Wide::MemoryAdd | Wide::MemoryMask => {
            // Read-modify-write of one word at r[d] + (h & 31) * 4.
            t.r(d);
            t.body.u32((h & 31) * 4).op(I32_ADD).set(ADDR);
            t.fast(0, 4, 4, true);
            t.body.if_();
            t.load_at(0, 4, false, VALUE);
            t.body.get(VALUE);
            if matches!(decode.wide(h, x), Wide::MemoryAdd) {
                t.body.i32(signed(x & 4095, 12)).op(I32_ADD);
            } else {
                let value = packed(x);
                match h & 0xc0 {
                    0 => t.body.u32(value).op(I32_OR),
                    0x80 => t.body.u32(value).op(I32_AND),
                    _ => t.body.u32(!value).op(I32_AND),
                };
            }
            t.body.set(VALUE);
            t.store_at(0, 4, VALUE);
            t.body.else_();
            t.exec(pc);
            t.body.end();
            Some((Flow::Continue, 4))
        }
        Wide::StoreImmediate => {
            t.r(d);
            t.body.u32((h & 31) * 4).op(I32_ADD).set(ADDR);
            t.fast(0, 4, 4, true);
            t.body.if_().u32(packed(x)).set(VALUE);
            t.store_at(0, 4, VALUE);
            t.body.else_();
            t.exec(pc);
            t.body.end();
            Some((Flow::Continue, 4))
        }
        Wide::StackPair => {
            let r = d & 14;
            t.sr(14);
            t.body.u32(x & 4092).op(I32_ADD).set(ADDR);
            let store = x & 1 != 0;
            t.fast(0, 8, 4, store);
            t.body.if_();
            if store {
                t.r(r);
                t.body.set(VALUE);
                t.store_at(0, 4, VALUE);
                t.r(r + 1);
                t.body.set(VALUE);
                t.store_at(4, 4, VALUE);
            } else {
                t.load_at(0, 4, false, LEFT);
                t.load_at(4, 4, false, RIGHT);
                t.set_r(r, LEFT);
                t.set_r(r + 1, RIGHT);
            }
            t.body.else_();
            t.exec(pc);
            t.body.end();
            Some((Flow::Continue, 4))
        }
        Wide::AddImmediate => {
            let value = match (h >> 4) & 15 {
                0 => x & 4095,
                1 => (x & 4095) + 4096,
                2 => (x & 4095) | 0xffffe000,
                3 => (x & 4095) | 0xfffff000,
                _ => packed(x),
            };
            t.r(d);
            t.body.set(LEFT).u32(value).set(RIGHT);
            t.arithmetic(n, false);
            Some((Flow::Continue, 4))
        }
        Wide::LogicImmediate => {
            let mode = (h >> 4) & 15;
            let value = if mode == 6 && x & 0xc00 == 0 {
                x & 1023
            } else {
                packed(x)
            };
            t.r(d);
            match mode {
                4 => t.body.u32(value).op(I32_OR),
                5 => t.body.u32(value).op(I32_XOR),
                6 => t.body.u32(value).op(I32_AND),
                _ => t.body.u32(!value).op(I32_AND),
            };
            t.body.set(VALUE);
            t.set_r(n, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::ShiftExtended => {
            let shift = ((x >> 8) & 3) * 16 + (x & 15);
            match (x >> 10) & 3 {
                0 | 2 if shift >= 32 => {
                    t.body.i32(0);
                }
                0 => {
                    t.r(s);
                    t.body.u32(shift).op(I32_SHL);
                }
                2 => {
                    t.r(s);
                    t.body.u32(shift).op(I32_SHR_U);
                }
                _ => {
                    t.r(s);
                    t.body.u32(shift.min(31)).op(I32_SHR_S);
                }
            }
            t.body.set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::ShiftRegisterExtended if x & 3 != 1 => {
            t.r(s);
            t.body.set(LEFT);
            t.r(c);
            t.body.set(RIGHT);
            if x & 3 == 3 {
                // ASR by min(count, 31).
                t.body
                    .get(LEFT)
                    .get(RIGHT)
                    .i32(31)
                    .get(RIGHT)
                    .i32(31)
                    .op(I32_LT_U)
                    .op(SELECT)
                    .op(I32_SHR_S);
            } else {
                t.body
                    .get(LEFT)
                    .get(RIGHT)
                    .op(if x & 3 == 0 { I32_SHL } else { I32_SHR_U })
                    .i32(0)
                    .get(RIGHT)
                    .u32(32)
                    .op(I32_LT_U)
                    .op(SELECT);
            }
            t.body.set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::RotateRightImmediate => {
            t.r(s);
            t.body
                .u32(((x >> 8) & 1) * 16 + (x & 15))
                .op(I32_ROTR)
                .set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::LogicThree => {
            t.r(s);
            t.r(c);
            match x & 3 {
                0 => t.body.op(I32_OR),
                1 => t.body.op(I32_XOR),
                2 => t.body.op(I32_AND),
                _ => t.body.i32(-1).op(I32_XOR).op(I32_AND),
            };
            t.body.set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::MultiplyImmediate => {
            t.r(d);
            t.body.u32(packed(x)).op(I32_MUL).set(VALUE);
            t.set_r(n, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::ArithmeticRegister if h == 0xe0b4 => {
            t.r(s);
            t.body.set(LEFT);
            t.r(c);
            t.body.set(RIGHT);
            t.arithmetic(d, x & 2 != 0);
            Some((Flow::Continue, 4))
        }
        Wide::BitMask => {
            t.r(s);
            t.body.i32(1);
            t.r(c);
            t.body.op(I32_SHL);
            match x & 3 {
                0 => t.body.op(I32_OR),
                1 => t.body.op(I32_XOR),
                2 => t.body.op(I32_AND),
                _ => t.body.i32(-1).op(I32_XOR).op(I32_AND),
            };
            t.body.set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::AddSpExtended => {
            t.sr(14);
            t.body.i32(signed(x, 13)).op(I32_ADD).set(VALUE);
            t.set_sr(14, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::AddStackExtended => {
            t.sr(14);
            t.body.u32(x & 4095).op(I32_ADD).set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        _ => {
            t.exec(pc);
            None
        }
    }
}
