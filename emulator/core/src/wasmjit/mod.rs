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

/// Where the translated code finds guest state in linear memory, and the
/// facts it builds in (rebuild translations when they change).
#[derive(Clone, Debug)]
pub struct Layout {
    /// `r[0..16]` as consecutive u32 words.
    pub r: u32,
    /// `sr[0..16]` as consecutive u32 words.
    pub sr: u32,
    /// The guest PC word.
    pub pc: u32,
    /// Byte 0 of guest SRAM (guest address `RAM`).
    pub ram: u32,
    /// Bus write counter, incremented by translated SRAM stores.
    pub writes: u32,
    /// The interrupt-enable flag (one byte, 0 or 1).
    pub interrupts: u32,
    /// The CPU write-protection windows: three inclusive `(low, high)` u32
    /// pairs, unused ones `(0, 0)` (see `Bus::guard_table`).
    pub guards: u32,
    /// Side-effect-free XIP reads: guest `[start, end)` at host `host +
    /// (address - start)`, for each segment.
    pub xip: Vec<(u32, u32, u32)>,
    /// Block chaining: a table of `(pc, function table index)` u32 pairs
    /// indexed by `(pc >> 1) & mask`, and the mask. When set, a region that
    /// leaves for a PC with a matching entry tail-calls that block directly
    /// (modules then import `env.table`, the host's function table).
    pub chain: Option<(u32, u32)>,
    /// SRAM code marks (one byte per `CODE_CHUNK` bytes, see
    /// `Bus::code_marks`): translated stores to marked SRAM take the slow
    /// path, which drops the stale translations.
    pub code_marks: Option<u32>,
    /// For code translated from SRAM: the NOR generation word and its value
    /// at translation. Inlined flash reads require it unchanged, since
    /// such code may run while XIP is remapped or off.
    pub xip_check: Option<(u32, u32)>,
}

/// The code area holding `pc` (XIP flash or SRAM) as `[start, end)`.
pub fn code_region(pc: u32) -> Option<(u32, u32)> {
    [(XIP, XIP_END), (RAM, RAM + RAM_SIZE as u32)]
        .into_iter()
        .find(|(start, end)| (*start..*end).contains(&pc))
}

pub struct Block {
    pub start: u32,
    /// Guest instructions in the longest path through the block.
    pub instructions: u32,
    pub wasm: Vec<u8>,
    /// Instruction bytes read for each basic block, `[start, end)`.
    pub ranges: Vec<(u32, u32)>,
}

/// Longest straight-line run translated into one basic block.
const MAX_INSTRUCTIONS: u32 = 48;
/// Basic blocks and instructions in one translated region. Larger regions
/// are no faster but cost engines (WebKit above all) far more memory and
/// executable code to compile, as regions entered at different PCs overlap.
const MAX_BLOCKS: usize = 16;
const MAX_REGION_INSTRUCTIONS: u32 = 256;
/// A region returns to the host at an internal branch once it has run this
/// many instructions, so devices and interrupts stay current.
const BUDGET: u32 = 256;

// Locals.
/// Instructions completed in earlier basic blocks of this call (the
/// function's parameter: chained blocks receive the running total).
const COUNT: u32 = 0;
const ADDR: u32 = 1;
const VALUE: u32 = 2;
const LEFT: u32 = 3;
const RIGHT: u32 = 4;
const RESULT: u32 = 5;
const WRITEBACK: u32 = 6;
/// Index of the next basic block, for the region's dispatch loop.
const NEXT_BLOCK: u32 = 7;
/// Parallel bundles: incoming registers (r0..r15, sr0..sr15) and the
/// following slot's results.
const SNAPSHOT: u32 = 8;
const FOLLOWING: u32 = SNAPSHOT + 32;
/// Guest registers cached for the whole region: r0..r15, then sr0..sr15
/// (only those the region uses are loaded and stored back).
const REG: u32 = FOLLOWING + 32;
const LOCALS: u32 = REG + 32;

/// The imported interpreter step, and its form for an instruction inside the
/// selected arm of a conditional block: (then_end, end) -> i32.
const EXEC: u32 = 0;
const EXEC_PREDICATED: u32 = 1;

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
    /// Instructions of the current basic block emitted before this one.
    count: u32,
    /// Open `if` constructs around the current code.
    depth: u32,
    /// Region blocks by start PC, and the index of the current one; `None`
    /// while scanning.
    region: Option<(&'a std::collections::HashMap<u32, usize>, usize, usize)>,
    /// Static successors seen while scanning.
    successors: Vec<u32>,
    /// Inside the selected arm of a conditional block: its (then_end, end).
    predicate: Option<(u32, u32)>,
    /// Whether any instruction was handed to the interpreter.
    used_exec: bool,
    /// Inside a slot of the parallel bundle at this PC: the interpreter must
    /// run the whole bundle, and the block ends after it.
    bundle: Option<u32>,
    /// Registers (bit i: r[i], bit 16 + i: sr[i]) read and written by the
    /// code emitted so far.
    used: u32,
    written: u32,
    /// Registers held in locals (from the scan pass): loaded on entry and
    /// after the interpreter ran, and the subset stored back to the CPU
    /// before it runs or the region returns.
    cached: u32,
    dirty: u32,
    /// PSR flag liveness: the events of the current basic block (from the
    /// scan pass), the block's start and the instruction being emitted.
    flags: Vec<FlagEvent>,
    block_start: u32,
    pc: u32,
    /// Flag updates (by `FlagEvent::Write` key) that a later update in the
    /// same basic block overwrites before anything can read them.
    dead_flags: std::collections::HashSet<(u32, u32, u32)>,
    /// Emitting nested code (a conditional arm or a bundle slot), where
    /// every flag update is kept.
    nested_code: bool,
}

/// What a basic block does with the PSR flags, in emission order.
#[derive(Clone, Copy, PartialEq)]
enum FlagEvent {
    /// NZCV recomputed: (block start, instruction PC, update index within
    /// the instruction).
    Write((u32, u32, u32)),
    /// PSR read, or code that may read it (the interpreter, any exit).
    Read,
}

impl<'a> Translator<'a> {
    fn new(layout: &'a Layout) -> Self {
        Self {
            body: Body::default(),
            layout,
            count: 0,
            depth: 0,
            region: None,
            successors: Vec::new(),
            predicate: None,
            used_exec: false,
            bundle: None,
            used: 0,
            written: 0,
            cached: 0,
            dirty: 0,
            flags: Vec::new(),
            block_start: 0,
            pc: 0,
            dead_flags: Default::default(),
            nested_code: false,
        }
    }
    /// A translator for code nested inside this one's current position.
    fn nested(&self, depth: u32) -> Self {
        Self {
            body: Body::default(),
            layout: self.layout,
            count: self.count,
            depth: self.depth + depth,
            region: self.region,
            successors: Vec::new(),
            predicate: self.predicate,
            used_exec: false,
            bundle: self.bundle,
            used: 0,
            written: 0,
            cached: self.cached,
            dirty: self.dirty,
            flags: Vec::new(),
            block_start: self.block_start,
            pc: self.pc,
            dead_flags: Default::default(),
            nested_code: true,
        }
    }
    /// Take on the register use of code nested in this translator.
    fn absorb(&mut self, nested: &Self) {
        self.used |= nested.used;
        self.written |= nested.written;
    }
}

impl Translator<'_> {
    fn r(&mut self, n: usize) {
        self.used |= 1 << n;
        self.body.get(REG + n as u32);
    }
    fn sr(&mut self, n: usize) {
        if n == 5 {
            self.flags.push(FlagEvent::Read);
        }
        self.used |= 1 << (16 + n);
        self.body.get(REG + 16 + n as u32);
    }
    /// `r[n] = local`.
    fn set_r(&mut self, n: usize, local: u32) {
        self.written |= 1 << n;
        self.body.get(local).set(REG + n as u32);
    }
    fn set_sr(&mut self, n: usize, local: u32) {
        self.written |= 1 << (16 + n);
        self.body.get(local).set(REG + 16 + n as u32);
    }
    /// `r[n] = constant`.
    fn set_r_const(&mut self, n: usize, value: u32) {
        self.written |= 1 << n;
        self.body.u32(value).set(REG + n as u32);
    }
    /// CPU address of cached register `i` (see `REG`).
    fn register_address(&self, i: u32) -> u32 {
        if i < 16 {
            self.layout.r + 4 * i
        } else {
            self.layout.sr + 4 * (i - 16)
        }
    }
    /// Load the cached registers from the CPU.
    fn reload(&mut self) {
        for i in (0..32).filter(|i| self.cached & (1 << i) != 0) {
            let address = self.register_address(i);
            self.body.i32(0).load(4, false, address).set(REG + i);
        }
    }
    /// Store the cached registers the region may change back to the CPU
    /// (before every exit and interpreter call, so it also reads the flags).
    fn flush(&mut self) {
        self.flags.push(FlagEvent::Read);
        for i in (0..32).filter(|i| self.dirty & (1 << i) != 0) {
            let address = self.register_address(i);
            self.body.i32(0).get(REG + i).store(4, address);
        }
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
    /// Return after the current instruction, which ran to completion, with
    /// the CPU's registers already current.
    fn finish_flushed(&mut self) {
        self.body.get(COUNT).u32(self.count + 1).op(I32_ADD).ret();
    }
    /// Store the registers and go on at the new PC: straight to its
    /// translated block when there is one and the budget allows, else by
    /// returning after the current instruction.
    fn leave(&mut self) {
        self.flush();
        self.leave_flushed();
    }
    fn leave_flushed(&mut self) {
        self.body.get(COUNT).u32(self.count + 1).op(I32_ADD).set(COUNT);
        self.chain();
        self.body.get(COUNT).ret();
    }
    /// With COUNT up to date and PC stored: tail-call the block for PC if
    /// the chain table has it and the budget is not spent.
    fn chain(&mut self) {
        let Some((table, mask)) = self.layout.chain else { return };
        self.body.get(COUNT).u32(BUDGET).op(I32_LT_U).if_();
        self.body
            .i32(0)
            .load(4, false, self.layout.pc)
            .set(ADDR)
            .get(ADDR)
            .i32(1)
            .op(I32_SHR_U)
            .u32(mask)
            .op(I32_AND)
            .i32(3)
            .op(I32_SHL)
            .set(VALUE)
            .get(VALUE)
            .load(4, false, table)
            .get(ADDR)
            .op(I32_EQ)
            .if_()
            .get(COUNT)
            .get(VALUE)
            .load(4, false, table + 4)
            .return_call_indirect(BLOCK_TYPE, 0)
            .end()
            .end();
    }
    /// Continue at a static `target` after the current instruction: inside
    /// the region by its dispatch loop (unless the budget is spent), else by
    /// returning to the host.
    fn goto(&mut self, target: u32) {
        self.successors.push(target);
        let Some((index, current, blocks)) = self.region else {
            self.set_pc_const(target);
            self.leave();
            return;
        };
        let Some(&block) = index.get(&target) else {
            self.set_pc_const(target);
            self.leave();
            return;
        };
        self.body
            .get(COUNT)
            .u32(self.count + 1)
            .op(I32_ADD)
            .set(COUNT)
            .get(COUNT)
            .u32(BUDGET)
            .op(I32_GE_U)
            .if_();
        self.set_pc_const(target);
        self.flush();
        self.body.get(COUNT).ret().end();
        self.body.u32(block as u32).set(NEXT_BLOCK);
        self.flags.push(FlagEvent::Read); // live into the next block
        // Blocks after the current one are still open around it.
        self.body.br(blocks as u32 - 1 - current as u32 + self.depth);
    }
    /// Run the current instruction in the interpreter and end the block.
    fn exec(&mut self, pc: u32) {
        self.used_exec = true;
        self.set_pc_const(self.bundle.unwrap_or(pc));
        self.flush();
        match self.predicate {
            Some((then_end, end)) => {
                self.body.u32(then_end).u32(end).call(EXEC_PREDICATED).op(DROP);
            }
            None => {
                // Chain on unless the interpreter asked to stop.
                self.body.call(EXEC).op(I32_EQZ).if_();
                self.leave_flushed();
                self.body.end();
            }
        }
        self.finish_flushed();
    }
    /// Run the current instruction (ending at `next`) in the interpreter; the
    /// block goes on with the next instruction unless the interpreter
    /// reports otherwise (fault, timed device access, timer poll, state that
    /// needs the interpreter) or control went elsewhere.
    fn exec_continue(&mut self, pc: u32, next: u32) {
        if self.bundle.is_some() || self.predicate.is_some() {
            self.exec(pc);
            return;
        }
        self.used_exec = true;
        self.set_pc_const(pc);
        self.flush();
        self.body.call(EXEC).set(RESULT).get(RESULT).op(I32_EQZ);
        self.body
            .i32(0)
            .load(4, false, self.layout.pc)
            .u32(next)
            .op(I32_EQ)
            .op(I32_AND)
            .op(I32_EQZ)
            .if_();
        // Control went elsewhere: chain on unless the interpreter stopped.
        self.body.get(RESULT).op(I32_EQZ).if_();
        self.leave_flushed();
        self.body.end();
        self.finish_flushed();
        self.body.end();
        self.reload();
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
            // Outside every write-protection window.
            for window in 0..3 {
                let address = self.layout.guards + 8 * window;
                self.body
                    .get(ADDR)
                    .i32(low)
                    .op(I32_ADD)
                    .i32(0)
                    .load(4, false, address + 4)
                    .op(I32_GT_U)
                    .get(ADDR)
                    .i32(high)
                    .op(I32_ADD)
                    .i32(0)
                    .load(4, false, address)
                    .op(I32_LE_U)
                    .op(I32_OR)
                    .op(I32_AND);
            }
            self.unmarked(low, high);
        }
    }
    /// AND onto the condition: `[ADDR + low, ADDR + high)` touches no SRAM
    /// chunk holding translated code (when SRAM code is translated).
    fn unmarked(&mut self, low: i32, high: i32) {
        let Some(marks) = self.layout.code_marks else { return };
        let chunks = RAM_SIZE as u32 / crate::bus::CODE_CHUNK;
        let mut offsets: Vec<i32> = (low..high).step_by(crate::bus::CODE_CHUNK as usize).collect();
        offsets.push(high - 1);
        for offset in offsets {
            // Masked so that it stays in bounds when ADDR is not SRAM (the
            // range test fails then anyway).
            self.body
                .get(ADDR)
                .i32(offset)
                .op(I32_ADD)
                .u32(RAM)
                .op(I32_SUB)
                .u32(crate::bus::CODE_CHUNK.trailing_zeros())
                .op(I32_SHR_U)
                .u32(chunks - 1)
                .op(I32_AND)
                .load(1, false, marks)
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

    fn mem(&mut self, pc: u32, next: u32, m: Mem) {
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
        let segments = if m.store {
            Vec::new()
        } else {
            self.layout.xip.clone()
        };
        // Constant data in flash: one nested test per mapped segment.
        for &(start, end, host) in &segments {
            self.body
                .get(ADDR)
                .u32(start)
                .op(I32_SUB)
                .u32(end - start - m.size as u32)
                .op(I32_LE_U);
            if m.size > 1 {
                self.body
                    .get(ADDR)
                    .u32(m.size as u32 - 1)
                    .op(I32_AND)
                    .op(I32_EQZ)
                    .op(I32_AND);
            }
            if let Some((generation, expected)) = self.layout.xip_check {
                self.body
                    .i32(0)
                    .load(4, false, generation)
                    .u32(expected)
                    .op(I32_EQ)
                    .op(I32_AND);
            }
            self.body
                .if_()
                .get(ADDR)
                .u32(start)
                .op(I32_SUB)
                .load(m.size, m.sign, host)
                .set(VALUE);
            self.set_r(m.reg, VALUE);
            if !matches!(m.writeback, Writeback::None) {
                self.set_base(m.base, WRITEBACK);
            }
            self.body.else_();
        }
        self.exec_continue(pc, next);
        for _ in &segments {
            self.body.end();
        }
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
            self.leave();
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
        let ordinal = self
            .flags
            .iter()
            .filter(|event| matches!(event, FlagEvent::Write((_, pc, _)) if *pc == self.pc))
            .count() as u32;
        let key = (self.block_start, self.pc, ordinal);
        if !self.nested_code {
            self.flags.push(FlagEvent::Write(key));
            if self.dead_flags.contains(&key) {
                // Overwritten before any read: skip computing it.
                #[cfg(test)]
                tests::SKIPPED_FLAGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.set_r(dest, RESULT);
                return;
            }
        }
        // VALUE = PSR with NZCV recomputed (keeping the other bits).
        self.used |= 1 << (16 + 5);
        self.body.get(REG + 16 + 5);
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
    translate_limited(bus, decode, layout, start, MAX_INSTRUCTIONS, MAX_BLOCKS)
}

/// Translate the region entered at `start`: up to `max_blocks` basic blocks
/// of at most `limit` instructions, reachable through static branches.
pub(crate) fn translate_limited(
    bus: &Bus,
    decode: &mut Decode,
    layout: &Layout,
    start: u32,
    limit: u32,
    max_blocks: usize,
) -> Option<Block> {
    let (area_start, area_end) = code_region(start)?;
    // Scan: find the blocks and their static successors.
    let mut blocks = vec![start];
    let mut total = 0;
    let mut next = 0;
    let (mut used, mut written) = (0, 0);
    let mut dead_flags = std::collections::HashSet::new();
    while next < blocks.len() {
        let mut scan = Translator::new(layout);
        let instructions = basic_block(&mut scan, bus, decode, blocks[next], limit);
        used |= scan.used;
        written |= scan.written;
        for pair in scan.flags.windows(2) {
            if let [FlagEvent::Write(key), FlagEvent::Write(_)] = pair {
                dead_flags.insert(*key);
            }
        }
        let Some(instructions) = instructions else {
            if next == 0 {
                return None;
            }
            // Unreadable code: leave it to the interpreter.
            blocks.remove(next);
            continue;
        };
        total += instructions;
        for successor in scan.successors {
            if blocks.len() < max_blocks
                && total < MAX_REGION_INSTRUCTIONS
                && (area_start..area_end).contains(&successor)
                && !blocks.contains(&successor)
            {
                blocks.push(successor);
            }
        }
        next += 1;
    }
    // Emit: a dispatch loop over the blocks, entered at block 0.
    let index: std::collections::HashMap<u32, usize> =
        blocks.iter().enumerate().map(|(i, &pc)| (pc, i)).collect();
    let mut ranges = Vec::new();
    let mut t = Translator::new(layout);
    // A register written on one path is stored back on every path, so it
    // must hold the CPU's value until then.
    t.cached = used | written;
    t.dirty = written;
    t.dead_flags = dead_flags;
    t.reload();
    let n = blocks.len();
    t.body.i32(0).set(NEXT_BLOCK).loop_();
    for _ in 0..n {
        t.body.block();
    }
    let targets: Vec<u32> = (0..n as u32).collect();
    t.body.get(NEXT_BLOCK).br_table(&targets, 0).end();
    for (current, &pc) in blocks.iter().enumerate() {
        t.region = Some((&index, current, n));
        bus.start_fetch_log();
        basic_block(&mut t, bus, decode, pc, limit)?;
        ranges.extend(bus.fetched());
        if current + 1 < n {
            t.body.end();
        }
    }
    t.body.end().unreachable();
    if (t.used | t.written) & !t.cached != 0 || t.written & !t.dirty != 0 {
        debug_assert!(false, "region at {start:08x} uses registers the scan missed");
        return None;
    }
    Some(Block {
        start,
        instructions: total,
        ranges,
        wasm: module(&t.body, LOCALS - 1, layout.chain.is_some()),
    })
}

/// Emit one basic block at `start`; it always ends by branching or
/// returning. Returns its instruction count.
fn basic_block(
    t: &mut Translator<'_>,
    bus: &Bus,
    decode: &mut Decode,
    start: u32,
    limit: u32,
) -> Option<u32> {
    t.count = 0;
    t.depth = 0;
    t.block_start = start;
    t.flags.clear();
    let mut pc = start;
    loop {
        t.pc = pc;
        let Ok(h) = bus.fetch(pc) else {
            if pc == start {
                return None;
            }
            // Let the interpreter raise the fetch fault.
            t.set_pc_const(pc);
            t.flush();
            t.body.get(COUNT).u32(t.count).op(I32_ADD).ret();
            return Some(t.count);
        };
        let flow = instruction(t, bus, decode, pc, h as u32);
        t.count += 1;
        let Some((flow, length)) = flow else {
            return Some(t.count);
        };
        pc = pc.wrapping_add(length);
        if flow == Flow::End {
            return Some(t.count);
        }
        if t.count >= limit || code_region(pc) != code_region(start) {
            // Continue at the next instruction as a separate block.
            t.count -= 1;
            t.goto(pc);
            t.count += 1;
            return Some(t.count);
        }
    }
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
        (code_region(address).is_some() && code_region(address) == code_region(pc))
            .then(|| bus.fetch(address).ok().map(u32::from))
            .flatten()
    };
    if h >> 13 == 6 || h & 0xf800 == 0xf000 {
        return parallel(t, bus, decode, pc, h);
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
                pc + 2,
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
            t.goto((pc + 6).wrapping_add(displacement));
            Some((Flow::End, 6))
        }
        First::Relative22 => {
            let displacement = signed(((h & 63) << 16) | word(2)?, 22) * 2;
            if h & 0xffc0 == 0xea80 {
                t.body.u32(pc + 4).set(VALUE);
                t.set_sr(3, VALUE);
            }
            t.goto((pc + 4).wrapping_add(displacement as u32));
            Some((Flow::End, 4))
        }
        First::CallRel9 => {
            let displacement = signed((((h >> 4) & 7) << 6) | (((h >> 8) & 31) << 1), 9);
            t.body.u32(next).set(VALUE);
            t.set_sr(3, VALUE);
            t.goto(next.wrapping_add(displacement as u32));
            Some((Flow::End, 2))
        }
        First::GotoRel12 => {
            let displacement = signed(
                ((h & 3) << 10) | (((h >> 4) & 15) << 6) | (((h >> 8) & 31) << 1),
                12,
            );
            t.goto(next.wrapping_add(displacement as u32));
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
            t.leave();
            Some((Flow::End, 2))
        }
        First::CallReg => {
            t.body.u32(next).set(VALUE);
            t.r(n);
            t.body.set(ADDR);
            t.set_sr(3, VALUE);
            t.set_pc_local(ADDR);
            t.leave();
            Some((Flow::End, 2))
        }
        First::SyncOrNop => Some((Flow::Continue, 2)),
        First::Sti | First::Cli => {
            let enable = matches!(decode.first(h), First::Sti);
            t.body.i32(0).i32(enable as i32).store(1, t.layout.interrupts);
            t.sr(11);
            if enable {
                t.body.u32(0x200).op(I32_OR);
            } else {
                t.body.u32(!0x200).op(I32_AND);
            }
            t.body.set(VALUE);
            t.set_sr(11, VALUE);
            if !enable {
                return Some((Flow::Continue, 2));
            }
            // A pending interrupt (such as an RTOS yield) is taken right
            // after STI: return to the host, which dispatches it.
            t.set_pc_const(pc + 2);
            t.flush();
            t.finish_flushed();
            Some((Flow::End, 2))
        }
        First::Extended(extended) => extended_instruction(t, bus, decode, pc, h, extended, word),
        _ => {
            t.exec(pc);
            None
        }
    }
}

/// Conditional branch on LEFT `cond` RIGHT.
fn branch(t: &mut Translator<'_>, cond: Cond, target: u32, fallthrough: u32) {
    t.body.get(LEFT).get(RIGHT).op(cond.opcode()).if_();
    t.depth += 1;
    t.goto(target);
    t.body.else_();
    t.goto(fallthrough);
    t.depth -= 1;
    t.body.end();
}

fn extended_instruction(
    t: &mut Translator<'_>,
    bus: &Bus,
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
                pc + 2,
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
            t.leave();
            Some((Flow::End, 2))
        }
        Extended::MemorySmall => {
            let size = if h & 0x2000 == 0 { 1 } else { 2 };
            t.mem(
                pc,
                pc + 2,
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
                pc + 2,
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
                pc + 2,
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
            wide_instruction(t, bus, decode, pc, h, x, &word)
        }
        _ => {
            t.exec(pc);
            None
        }
    }
}

fn wide_instruction(
    t: &mut Translator<'_>,
    bus: &Bus,
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
        t.mem(pc, pc + 4, m);
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
        kind @ (Wide::MemoryPair
        | Wide::PairPostincrement
        | Wide::PairIndexed
        | Wide::PairRegisterPreincrement) => {
            // Two words at ADDR (as `Cpu::execute_current`); some forms
            // also update r[s] to WRITEBACK.
            t.r(s);
            let writeback = match kind {
                Wide::MemoryPair => {
                    let offset =
                        (signed(h & 7, 3) << 8) | (((x >> 8) & 15) << 4) as i32 | (x & 12) as i32;
                    t.body.i32(offset).op(I32_ADD).set(ADDR);
                    false
                }
                Wide::PairPostincrement => {
                    let stride =
                        (signed(h & 7, 3) << 8) + ((((x >> 8) & 15) << 4) | (x & 12)) as i32;
                    t.body.set(ADDR).get(ADDR).i32(stride).op(I32_ADD).set(WRITEBACK);
                    true
                }
                Wide::PairIndexed => {
                    t.r(c);
                    if x & 8 != 0 {
                        t.body.i32(3).op(I32_SHL);
                    }
                    t.body.op(I32_ADD).set(ADDR);
                    false
                }
                _ => {
                    t.r(c);
                    t.body.op(I32_ADD).set(ADDR).get(ADDR).set(WRITEBACK);
                    true
                }
            };
            let reg = d & 14;
            let store = x & 1 != 0;
            t.fast(0, 8, 4, store);
            t.body.if_();
            if store {
                t.r(reg);
                t.body.set(VALUE);
                t.store_at(0, 4, VALUE);
                t.r(reg + 1);
                t.body.set(VALUE);
                t.store_at(4, 4, VALUE);
                if writeback {
                    t.set_r(s, WRITEBACK);
                }
            } else {
                t.load_at(0, 4, false, LEFT);
                t.load_at(4, 4, false, RIGHT);
                if writeback {
                    t.set_r(s, WRITEBACK);
                }
                t.set_r(reg, LEFT);
                t.set_r(reg + 1, RIGHT);
            }
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
        Wide::MultiplyExtended => {
            t.r(s);
            t.r(c);
            t.body.op(I32_MUL).set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::BitField => {
            let pos = (x >> 7) & 31;
            let len = (x >> 2) & 31;
            let mask = (1u32 << len).wrapping_sub(1);
            if h & 0x10 == 0 {
                t.r(n);
                t.body.u32(!(mask << pos)).op(I32_AND);
                t.r(d);
                t.body.u32(mask).op(I32_AND).u32(pos).op(I32_SHL).op(I32_OR);
            } else {
                t.r(d);
                t.body.u32(pos).op(I32_SHR_U).u32(mask).op(I32_AND);
            }
            t.body.set(VALUE);
            t.set_r(n, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::Absolute => {
            t.r(c);
            t.body
                .set(LEFT)
                .get(LEFT)
                .i32(31)
                .op(I32_SHR_S)
                .set(RIGHT)
                .get(LEFT)
                .get(RIGHT)
                .op(I32_XOR)
                .get(RIGHT)
                .op(I32_SUB)
                .set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::CountLeadingZeros => {
            t.r(c);
            t.body.op(I32_CLZ).set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::ReverseBytes => {
            t.r(c);
            t.body
                .set(LEFT)
                .get(LEFT)
                .u32(0xff00_ff00)
                .op(I32_AND)
                .u32(8)
                .op(I32_ROTL)
                .get(LEFT)
                .u32(0x00ff_00ff)
                .op(I32_AND)
                .u32(8)
                .op(I32_ROTR)
                .op(I32_OR)
                .set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::Maximum | Wide::Minimum => {
            let maximum = matches!(decode.wide(h, x), Wide::Maximum);
            t.r(s);
            t.body.set(LEFT);
            t.r(c);
            t.body.set(RIGHT);
            let compare = match (maximum, x & 1 != 0) {
                (true, false) => I32_GT_U,
                (true, true) => I32_GT_S,
                (false, false) => I32_LT_U,
                (false, true) => I32_LT_S,
            };
            t.body
                .get(LEFT)
                .get(RIGHT)
                .get(LEFT)
                .get(RIGHT)
                .op(compare)
                .op(SELECT)
                .set(VALUE);
            t.set_r(d, VALUE);
            Some((Flow::Continue, 4))
        }
        Wide::ReverseSubtract => {
            t.body.u32(packed(x)).set(LEFT);
            t.r(d);
            t.body.set(RIGHT);
            t.arithmetic(n, true);
            Some((Flow::Continue, 4))
        }
        Wide::SubtractPackedImmediate => {
            t.r(d);
            t.body.set(LEFT).u32(packed(x)).set(RIGHT);
            t.arithmetic(n, true);
            Some((Flow::Continue, 4))
        }
        Wide::BranchBit => {
            t.r(n);
            t.body
                .u32(1 << ((x >> 11) & 31))
                .op(I32_AND)
                .set(LEFT)
                .i32(0)
                .set(RIGHT);
            let cond = if x & 512 != 0 { Cond::Ne } else { Cond::Eq };
            branch(t, cond, next.wrapping_add((signed(x & 511, 9) * 2) as u32), next);
            Some((Flow::End, 4))
        }
        Wide::StackExtended => mem(
            t,
            Mem {
                reg: d,
                base: Base::Sp,
                offset: (x & 4092) as i32,
                index: None,
                size: 4,
                store: x & 1 != 0,
                sign: false,
                writeback: Writeback::None,
            },
        ),
        Wide::StackSubword => {
            let halfword = h & 4 == 0;
            mem(
                t,
                Mem {
                    reg: d,
                    base: Base::Sp,
                    offset: (if halfword { x & 4094 } else { x & 4095 }) as i32,
                    index: None,
                    size: if halfword { 2 } else { 1 },
                    store: if halfword {
                        h == 0xe9d8 && x & 1 != 0
                    } else {
                        h == 0xe9de
                    },
                    sign: h & 1 != 0,
                    writeback: Writeback::None,
                },
            )
        }
        Wide::MemoryShift | Wide::MemoryArithmeticRegister => {
            let shift_form = matches!(decode.wide(h, x), Wide::MemoryShift);
            t.r(d);
            t.body.u32(x & 252).op(I32_ADD).set(ADDR);
            t.fast(0, 4, 4, true);
            t.body.if_();
            t.load_at(0, 4, false, VALUE);
            t.body.get(VALUE);
            if shift_form {
                let shift = c as u32 + (h & 1) * 16;
                match x & 3 {
                    0 => t.body.u32(shift).op(I32_SHL),
                    2 => t.body.u32(shift).op(I32_SHR_U),
                    _ => t.body.u32(shift).op(I32_SHR_S),
                };
            } else {
                t.r(c);
                t.body.op(if x & 2 != 0 { I32_SUB } else { I32_ADD });
            }
            t.body.set(VALUE);
            t.store_at(0, 4, VALUE);
            t.body.else_();
            t.exec(pc);
            t.body.end();
            Some((Flow::Continue, 4))
        }
        Wide::ConditionalBlock => conditional(t, bus, decode, pc, h, x),
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

/// Length of the instruction at `h` as conditional blocks count it.
fn counted_length(h: u32) -> u32 {
    if matches!(h & 0xffe0, 0xffc0 | 0xffe0) || h == 0xff80 {
        6
    } else if h >> 13 == 7 {
        4
    } else {
        2
    }
}

/// `(then_end, end)` of the conditional block at `pc` with operand `x`,
/// walking its arms as `Cpu::conditional` does.
fn conditional_extent(bus: &Bus, pc: u32, x: u32) -> Option<(u32, u32)> {
    let mut cursor = pc + 4;
    let mut then_end = cursor;
    let then_count = (x >> 14) + 1;
    let else_count = (x >> 12) & 3;
    for i in 0..then_count + else_count {
        let h = bus.fetch(cursor).ok()? as u32;
        cursor += counted_length(h);
        if h >> 13 == 6 || h & 0xf800 == 0xf000 {
            cursor += counted_length(bus.fetch(cursor).ok()? as u32);
        }
        if i + 1 == then_count {
            then_end = cursor;
        }
    }
    Some((then_end, cursor))
}

/// A conditional block: when the test holds, run the then arm and continue
/// at `end`; otherwise continue at the else arm (`then_end`). The then arm
/// is translated inline when every instruction in it falls through;
/// instructions it hands to the interpreter carry the predicate along.
fn conditional(
    t: &mut Translator<'_>,
    bus: &Bus,
    decode: &mut Decode,
    pc: u32,
    h: u32,
    x: u32,
) -> Option<(Flow, u32)> {
    let kind = (h >> 4) & 255;
    let n = (h & 15) as usize;
    let c = ((x >> 8) & 15) as usize;
    let float = matches!(kind, 0xd1 | 0xd9 | 0xe1 | 0xe9) && x & 128 != 0;
    let extent = conditional_extent(bus, pc, x);
    let Some((then_end, end)) = extent.filter(|_| !float && t.predicate.is_none()) else {
        t.exec(pc);
        return None;
    };
    // The then arm, translated separately so it can be rejected.
    let mut arm = t.nested(1);
    arm.predicate = Some((then_end, end));
    let mut cursor = pc + 4;
    while cursor < then_end {
        arm.count += 1;
        let Ok(word) = bus.fetch(cursor) else {
            break;
        };
        match instruction(&mut arm, bus, decode, cursor, word as u32) {
            Some((Flow::Continue, length)) => cursor += length,
            // A branch or call ending a block without an else arm: the
            // skip at then_end would go to then_end, so none is pending.
            Some((Flow::End, length)) if cursor + length == then_end && then_end == end => {
                cursor += length;
                break;
            }
            _ => break,
        }
    }
    t.absorb(&arm);
    t.flags.push(FlagEvent::Read); // the arm keeps and may read the flags
    if cursor != then_end {
        t.exec(pc);
        return None;
    }
    // LEFT, RIGHT: operands; VALUE: whether the test holds.
    t.r(n);
    t.body.set(LEFT);
    if kind & 7 == 1 {
        t.r(c);
    } else if matches!(kind, 0x93 | 0x9b | 0xc3 | 0xcb) {
        t.body.u32(x & 4095);
    } else if matches!(kind, 0x83 | 0x8b | 0xd3 | 0xdb | 0xe3 | 0xeb) {
        t.body.i32(signed(x & 4095, 12));
    } else {
        t.body.u32(packed(x));
    }
    t.body.set(RIGHT);
    let compare = |opcode: u8| (opcode, false);
    let (opcode, masked) = match kind {
        0x81..=0x83 => compare(I32_EQ),
        0x89..=0x8b => compare(I32_NE),
        0x91..=0x93 => compare(I32_GE_U),
        0x99..=0x9b => compare(I32_LT_U),
        0xa1 => (if x & 128 == 0 { I32_EQ } else { I32_NE }, true),
        0xa2 => (I32_EQ, true),
        0xa3 => (I32_NE, true),
        0xc1..=0xc3 => compare(I32_GT_U),
        0xc9..=0xcb => compare(I32_LE_U),
        0xd1..=0xd3 => compare(I32_GE_S),
        0xd9..=0xdb => compare(I32_LT_S),
        0xe1..=0xe3 => compare(I32_GT_S),
        _ => compare(I32_LE_S),
    };
    if masked {
        t.body.get(LEFT).get(RIGHT).op(I32_AND).i32(0).op(opcode);
    } else {
        t.body.get(LEFT).get(RIGHT).op(opcode);
    }
    let condition_count = t.count;
    t.body.if_();
    t.body.bytes.extend_from_slice(&arm.body.bytes);
    t.depth += 1;
    t.count = arm.count;
    t.goto(end);
    t.body.else_();
    t.count = condition_count;
    t.goto(then_end);
    t.depth -= 1;
    t.body.end();
    Some((Flow::End, 4))
}

/// A parallel bundle: both slots read the incoming registers; the following
/// slot runs first, and its results stand wherever the primary slot left a
/// register unchanged (as `Cpu::execute_current`). Only bundles whose slots
/// are register operations that fall through are translated.
fn parallel(
    t: &mut Translator<'_>,
    bus: &Bus,
    decode: &mut Decode,
    pc: u32,
    h: u32,
) -> Option<(Flow, u32)> {
    let length = if h >> 13 == 6 { 2 } else { 4 };
    let normalized = if length == 2 { h & 0x1fff } else { h & !0x1000 };
    let following_pc = pc + length;
    let following = bus.fetch(following_pc).ok().map(u32::from);
    let mut second = t.nested(0);
    second.bundle = Some(pc);
    let mut first = t.nested(0);
    first.bundle = Some(pc);
    let slots = following.and_then(|word| {
        let (flow, following_length) = instruction(&mut second, bus, decode, following_pc, word)?;
        let (primary_flow, _) = instruction(&mut first, bus, decode, pc, normalized)?;
        // A slot handed to the interpreter reruns the whole bundle from the
        // incoming state, so at most one slot may need it (the following
        // slot runs first; the primary sees the restored registers).
        (flow == Flow::Continue
            && primary_flow == Flow::Continue
            && !(second.used_exec && first.used_exec)
            && word >> 13 != 6
            && word & 0xf800 != 0xf000)
            .then_some(following_length)
    });
    t.absorb(&second);
    t.absorb(&first);
    t.flags.push(FlagEvent::Read); // slots keep and may read the flags
    let Some(following_length) = slots else {
        t.exec(pc);
        return None;
    };
    // Registers either slot touches; the others keep their values.
    let touched: Vec<u32> = (0..32)
        .filter(|i| (second.used | second.written | first.used | first.written) & (1 << i) != 0)
        .collect();
    for &i in &touched {
        t.body.get(REG + i).set(SNAPSHOT + i);
    }
    t.body.bytes.extend_from_slice(&second.body.bytes);
    for &i in &touched {
        t.body.get(REG + i).set(FOLLOWING + i);
        t.body.get(SNAPSHOT + i).set(REG + i);
    }
    t.body.bytes.extend_from_slice(&first.body.bytes);
    for &i in &touched {
        // Keep the primary's write; else take the following slot's value.
        t.body
            .get(FOLLOWING + i)
            .get(REG + i)
            .get(REG + i)
            .get(SNAPSHOT + i)
            .op(I32_EQ)
            .op(SELECT)
            .set(REG + i);
    }
    Some((Flow::Continue, length + following_length))
}
