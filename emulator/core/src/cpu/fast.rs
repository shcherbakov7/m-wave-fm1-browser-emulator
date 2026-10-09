// SPDX-License-Identifier: GPL-3.0-only
// Batched execution of simple, prepared instructions. While only one core has
// work and no repeat or conditional block is active, straight-line register,
// SRAM and branch instructions from XIP run back to back. Guest time, devices
// and interrupt dispatch are brought up to date once per batch, so an
// interrupt can be at most `BATCH` instructions late. Anything else (MMIO,
// complex or parallel forms, RAM-resident code) goes through `Cpu::step`.
use super::{Cpu, Fault};
use crate::blocks::{prepare, Instruction, Op};

const SLOTS: usize = 1 << 14;
/// Longest batch between device updates and interrupt checks.
const BATCH: u64 = 64;

#[derive(Clone, Copy)]
struct Entry {
    pc: u32,
    generation: u32,
    /// `None` when the instruction at `pc` is not batchable.
    instruction: Option<Instruction>,
}

pub(crate) struct Cache {
    entries: Box<[Entry]>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: vec![
                Entry {
                    pc: u32::MAX,
                    generation: 0,
                    instruction: None,
                };
                SLOTS
            ]
            .into_boxed_slice(),
        }
    }
}

impl Cpu {
    /// Execute `budget` steps, batching simple instructions where possible.
    /// Equivalent to calling `step` `budget` times, except that interrupts
    /// and device-visible time are only checked between batches.
    pub fn step_many(&mut self, budget: u64) -> Result<(), Fault> {
        let end = self.steps.saturating_add(budget);
        while self.steps < end {
            if self.batch_ready() && self.batch((end - self.steps).min(BATCH))? != 0 {
                continue;
            }
            let op = self.step()?;
            if let Some(ops) = &mut self.unbatched_ops {
                *ops.entry(op).or_default() += 1;
            }
        }
        Ok(())
    }

    /// Whether batched (and translated) execution may run now: one core
    /// has work and no IDLE or hold-off is pending.
    pub fn batch_ready(&self) -> bool {
        if self.idle || self.bus_locked || self.idle_wake_delay != 0 || !self.time_warp {
            return false;
        }
        let control = self.bus.core_control(1);
        match &self.secondary {
            // `step` would start the secondary core from its handoff vector.
            None => control & 10 != 8,
            Some(core) => {
                let running = control & 0x18 == 8;
                control & 2 == 0
                    && self.bus.core_control(0) & 16 == 0
                    && (!running || (core.idle && !core.bus_locked))
            }
        }
    }

    fn prepared(&mut self, pc: u32) -> Option<Instruction> {
        if !(crate::XIP..crate::XIP_END).contains(&pc) {
            return None;
        }
        let generation = self.bus.code_generation();
        let slot = (pc >> 1) as usize & (SLOTS - 1);
        let entry = self.fast.entries[slot];
        if entry.pc == pc && entry.generation == generation {
            return entry.instruction;
        }
        let instruction = self
            .bus
            .code(pc)
            .ok()
            .and_then(|h| prepare(&self.bus, &mut self.decode, pc, h))
            .filter(|i| !matches!(i.op, Op::Fallback));
        self.fast.entries[slot] = Entry {
            pc,
            generation,
            instruction,
        };
        instruction
    }

    /// Run up to `max` instructions; returns how many ran. Prepared simple
    /// forms run directly, everything else through the interpreter. A batch
    /// ends after any device-register access, so devices lag by at most the
    /// batch, and after a timer poll so waits can be fast-forwarded.
    fn batch(&mut self, max: u64) -> Result<u64, Fault> {
        let start_pc = self.pc;
        let mut count = 0;
        let mut poll_pc = None;
        while count < max && !self.idle {
            let devices = self.bus.device_accesses.get();
            let polls = self.bus.timer_polls.get();
            let pc = self.pc;
            let prepared = if self.predicate_skip.is_none() && self.repeat.is_none() {
                self.prepared(pc)
            } else {
                None
            };
            match prepared {
                Some(instruction) => {
                    instruction.execute(self)?;
                }
                None => {
                    self.execute_current()?;
                }
            }
            count += 1;
            if self.bus.timer_polls.get() != polls {
                poll_pc = Some(pc);
                break;
            }
            if self.bus.device_accesses.get() != devices {
                break;
            }
        }
        self.finish_batch(count, start_pc, poll_pc)?;
        Ok(count)
    }

    /// Account for `count` instructions run outside `step` (by a batch or a
    /// translated block) starting at `start_pc`: advance guest time and
    /// devices, fast-forward a wait if the batch ended on a timer poll at
    /// `poll_pc`, and dispatch a pending interrupt.
    pub fn finish_batch(
        &mut self,
        count: u64,
        start_pc: u32,
        poll_pc: Option<u32>,
    ) -> Result<(), Fault> {
        if count == 0 {
            return Ok(());
        }
        self.steps += count;
        self.batched_steps += count;
        let ticks = self.bus.instruction_ticks_n(count) as u32 + self.warp_ticks(poll_pc);
        self.advance_time(ticks, start_pc)?;
        self.idle_wake_delay = self.idle_wake_delay.saturating_sub(count.min(255) as u8);
        self.dispatch_interrupt()
    }

    /// Run the instruction at PC in the interpreter, without time or
    /// interrupts. Returns its PC and operation name.
    pub fn interpret(&mut self) -> Result<(u32, &'static str), Fault> {
        self.execute_current()
    }

    /// Run the instruction at PC for a translated block, which continues
    /// with the next instruction only if this returns `false`: there was no
    /// timed device access or timer poll and the interpreter is not needed
    /// next (the block also checks that control fell through).
    pub fn interpret_for_block(&mut self) -> Result<(u32, &'static str, bool), Fault> {
        let counters = self.batch_counters();
        let (pc, name) = self.execute_current()?;
        let stop = self.batch_counters() != counters || self.needs_interpreter();
        Ok((pc, name, stop))
    }

    /// Run the instruction at PC, which lies in the selected arm of a
    /// conditional block spanning to `end` with the arm ending at `then_end`.
    pub fn interpret_predicated(
        &mut self,
        then_end: u32,
        end: u32,
    ) -> Result<(u32, &'static str), Fault> {
        self.predicate_skip = Some((then_end, end));
        self.execute_current()
    }

    pub(crate) fn predicate(&self) -> Option<(u32, u32)> {
        self.predicate_skip
    }

    /// Counters a batch watches: device-register accesses and timer polls.
    pub fn batch_counters(&self) -> (u64, u64) {
        (self.bus.device_accesses.get(), self.bus.timer_polls.get())
    }

    /// Whether only the interpreter may run the next instruction (a
    /// conditional block or repeat loop is in progress, or the core idles).
    pub fn needs_interpreter(&self) -> bool {
        self.idle || self.predicate_skip.is_some() || self.repeat.is_some()
    }

    pub fn is_idle(&self) -> bool {
        self.idle
    }

    /// Translate the block at `pc` for state at the addresses of this CPU.
    pub fn translate_block(&mut self, pc: u32) -> Option<crate::wasmjit::Block> {
        let layout = self.jit_layout();
        crate::wasmjit::translate(&self.bus, &mut self.decode, &layout, pc)
    }

    /// Addresses of the state translated code works on. Valid while this
    /// CPU stays in place (it must not be moved after translating).
    pub fn jit_layout(&self) -> crate::wasmjit::Layout {
        let address = |pointer: *const u8| pointer as usize as u32;
        crate::wasmjit::Layout {
            r: address(self.r.as_ptr().cast()),
            sr: address(self.sr.as_ptr().cast()),
            pc: address((&self.pc as *const u32).cast()),
            ram: address(self.bus.ram().as_ptr()),
            writes: address((&self.bus.writes as *const u32).cast()),
            interrupts: address((&self.interrupts_enabled as *const bool).cast()),
            windows: self.bus.guard_windows(),
            xip: self
                .bus
                .xip_data_view()
                .into_iter()
                .map(|(start, end, host)| (start, end, address(host)))
                .collect(),
        }
    }
}
