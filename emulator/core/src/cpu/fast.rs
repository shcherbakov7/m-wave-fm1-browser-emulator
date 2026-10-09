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

    fn batch_ready(&self) -> bool {
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
        if count != 0 {
            self.steps += count;
            self.batched_steps += count;
            let ticks = self.bus.instruction_ticks_n(count) as u32 + self.warp_ticks(poll_pc);
            self.advance_time(ticks, start_pc)?;
            self.idle_wake_delay = self.idle_wake_delay.saturating_sub(count.min(255) as u8);
            self.dispatch_interrupt()?;
        }
        Ok(count)
    }
}
