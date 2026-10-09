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

/// Runs code for the instruction at the CPU's PC faster than the
/// interpreter (prepared forms, or translated blocks in the browser).
pub trait BlockRunner {
    /// Run code at `cpu.pc`, returning how many instructions ran and the PC
    /// of the last one, or `None` to let the interpreter run one instruction.
    /// It must leave state exactly as the interpreter would.
    fn run(&mut self, cpu: &mut Cpu) -> Option<Result<(u64, u32), Fault>>;
}

/// The built-in runner: one prepared simple instruction at a time.
pub struct PreparedRunner;

impl BlockRunner for PreparedRunner {
    fn run(&mut self, cpu: &mut Cpu) -> Option<Result<(u64, u32), Fault>> {
        let pc = cpu.pc;
        let instruction = cpu.prepared(pc)?;
        Some(instruction.execute(cpu).map(|_| (1, pc)))
    }
}

impl Cpu {
    /// Execute `budget` steps, batching simple instructions where possible.
    /// Equivalent to calling `step` `budget` times, except that interrupts
    /// and device-visible time are only checked between batches.
    pub fn step_many(&mut self, budget: u64) -> Result<(), Fault> {
        self.step_many_with(budget, &mut PreparedRunner)
    }

    /// As `step_many`, running code through `blocks` where it can. While
    /// both cores have work, each runs a batch in turn: the primary's
    /// instructions set the guest time and the secondary runs at most as
    /// many (it interleaves per batch rather than per instruction).
    pub fn step_many_with(&mut self, budget: u64, blocks: &mut dyn BlockRunner) -> Result<(), Fault> {
        let end = self.steps.saturating_add(budget);
        while self.steps < end {
            let max = (end - self.steps).min(BATCH);
            if self.batch_ready() {
                let start_pc = self.pc;
                let (count, poll_pc) = self.run_core(max, blocks)?;
                if count != 0 {
                    self.finish_batch(count, start_pc, poll_pc)?;
                    continue;
                }
            } else if self.dual_ready() && self.dual_batch(max, blocks)? != 0 {
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

    /// Whether both cores have work and may run batches in turn.
    fn dual_ready(&self) -> bool {
        let Some(core) = &self.secondary else {
            return false;
        };
        let control = self.bus.core_control(1);
        self.time_warp
            && control & 2 == 0
            && control & 0x18 == 8
            && self.bus.core_control(0) & 16 == 0
            && !self.idle
            && !core.idle
            && !self.bus_locked
            && !core.bus_locked
            && self.idle_wake_delay == 0
            && core.idle_wake_delay == 0
    }

    pub(crate) fn prepared(&mut self, pc: u32) -> Option<Instruction> {
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

    /// Run up to about `max` instructions on the current core without time
    /// or interrupts: through `blocks` where possible, else the interpreter.
    /// Stops after a timed device access, or after a timer poll (returning
    /// its PC so the wait can be fast-forwarded), or when the core idles.
    fn run_core(
        &mut self,
        max: u64,
        blocks: &mut dyn BlockRunner,
    ) -> Result<(u64, Option<u32>), Fault> {
        let mut count = 0;
        while count < max && !self.idle {
            let (devices, polls) = self.batch_counters();
            let pc = self.pc;
            let ran = if self.predicate_skip.is_none() && self.repeat.is_none() {
                blocks.run(self).transpose()?
            } else {
                None
            };
            let (executed, last_pc) = match ran {
                Some(ran) => ran,
                None => {
                    self.execute_current()?;
                    (1, pc)
                }
            };
            count += executed;
            let (now_devices, now_polls) = self.batch_counters();
            if now_polls != polls {
                return Ok((count, Some(last_pc)));
            }
            if now_devices != devices {
                break;
            }
        }
        Ok((count, None))
    }

    /// One batch on each core: the primary first, then the secondary for at
    /// most as many instructions; guest time follows the primary.
    fn dual_batch(&mut self, max: u64, blocks: &mut dyn BlockRunner) -> Result<u64, Fault> {
        let start_pc = self.pc;
        let (count, _) = self.run_core(max, blocks)?;
        if count == 0 {
            return Ok(0);
        }
        let mut secondary = self.secondary.take().unwrap();
        secondary.swap(self);
        let result = self.run_core(count, blocks).and_then(|(executed, _)| {
            self.steps += executed;
            self.batched_steps += executed;
            self.idle_wake_delay = self.idle_wake_delay.saturating_sub(executed.min(255) as u8);
            self.dispatch_interrupt()
        });
        secondary.swap(self);
        self.secondary = Some(secondary);
        result?;
        self.finish_batch(count, start_pc, None)?;
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

    #[cfg(test)]
    pub(crate) fn predicate(&self) -> Option<(u32, u32)> {
        self.predicate_skip
    }

    /// Register-level state a translated block may change (diagnostics):
    /// r, sr, pc, interrupt enable and the bus write counter.
    pub fn block_state(&self) -> ([u32; 16], [u32; 16], u32, bool, u32) {
        (
            self.r,
            self.sr,
            self.pc,
            self.interrupts_enabled,
            self.bus.writes,
        )
    }

    pub fn set_block_state(&mut self, state: ([u32; 16], [u32; 16], u32, bool, u32)) {
        (self.r, self.sr, self.pc, self.interrupts_enabled, self.bus.writes) = state;
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
