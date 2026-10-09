// SPDX-License-Identifier: GPL-3.0-only
//! Dispatcher for translated blocks (WebAssembly host only).
//!
//! Hot XIP blocks are translated by `fm1_emu::wasmjit`, compiled by the
//! JavaScript host (`env.jit_compile`, which instantiates the module and
//! appends its function to this module's indirect function table) and then
//! called through that table. Batches follow `Cpu::step_many`: devices,
//! guest time and interrupts are updated once per batch, and a batch ends
//! after a device access or timer poll.
use fm1_emu::cpu::Cpu;
use std::collections::HashMap;

#[link(wasm_import_module = "env")]
extern "C" {
    /// Compile a block module; returns its function's table index or -1.
    fn jit_compile(bytes: *const u8, length: usize) -> i32;
}

/// Executions before a block is translated.
const HOT: u32 = 16;
/// Instructions per batch before devices and interrupts are updated.
const BATCH: u64 = 256;

#[derive(Clone, Copy)]
enum Slot {
    Counting(u32),
    Ready(extern "C" fn() -> u32),
    Unavailable,
}

/// Direct-mapped block table: entries by `(pc >> 1) & (SLOTS - 1)`, tagged
/// with their PC. A colliding block simply replaces the previous entry.
const SLOTS: usize = 1 << 15;

/// State shared with `fm1_jit_exec`, which translated code calls back into.
struct Exec {
    cpu: *mut Cpu,
    pc: u32,
    fault: Option<String>,
}

static mut EXEC: Exec = Exec {
    cpu: std::ptr::null_mut(),
    pc: 0,
    fault: None,
};

/// Imported by translated blocks as `env.exec`: interpret the instruction at
/// PC. The block ends after it, whatever this returns.
#[no_mangle]
pub extern "C" fn fm1_jit_exec() -> i32 {
    // SAFETY: only reachable from a block called by `Jit::run`, which points
    // EXEC.cpu at the CPU it is running and holds no other reference to it
    // across the call. The host is single-threaded.
    unsafe {
        EXEC_CALLS += 1;
        let exec = &mut *std::ptr::addr_of_mut!(EXEC);
        let cpu = &mut *exec.cpu;
        match cpu.interpret_for_block() {
            Ok((pc, name, stop)) => {
                exec.pc = pc;
                if let Some(ops) = &mut *std::ptr::addr_of_mut!(EXEC_OPS) {
                    *ops.entry((name, pc)).or_default() += 1;
                }
                stop as i32
            }
            Err(error) => {
                exec.fault = Some(format!("{error} (after {} steps)", cpu.steps));
                1
            }
        }
    }
}

/// Imported by translated blocks as `env.exec_pred`: interpret the
/// instruction at PC inside the selected arm of a conditional block.
#[no_mangle]
pub extern "C" fn fm1_jit_exec_pred(then_end: u32, end: u32) -> i32 {
    // SAFETY: as for `fm1_jit_exec`.
    unsafe {
        EXEC_CALLS += 1;
        let exec = &mut *std::ptr::addr_of_mut!(EXEC);
        let cpu = &mut *exec.cpu;
        match cpu.interpret_predicated(then_end, end) {
            Ok((pc, name)) => {
                exec.pc = pc;
                if let Some(ops) = &mut *std::ptr::addr_of_mut!(EXEC_OPS) {
                    *ops.entry((name, pc)).or_default() += 1;
                }
            }
            Err(error) => exec.fault = Some(format!("{error} (after {} steps)", cpu.steps)),
        }
    }
    1
}

#[derive(Default)]
pub struct Stats {
    pub compiled: u32,
    pub failed: u32,
    pub translated_steps: u64,
    pub block_calls: u64,
    pub batches: u64,
}

/// Interpreter calls made by translated blocks.
pub static mut EXEC_CALLS: u64 = 0;
/// When profiling, interpreter calls from blocks by operation name and PC.
pub static mut EXEC_OPS: Option<HashMap<(&'static str, u32), u64>> = None;

pub struct Jit {
    slots: Vec<(u32, Slot)>,
    generation: u32,
    pub enabled: bool,
    pub stats: Stats,
}

impl Jit {
    pub fn new() -> Self {
        Self {
            slots: vec![(u32::MAX, Slot::Unavailable); SLOTS],
            generation: u32::MAX,
            enabled: true,
            stats: Stats::default(),
        }
    }

    fn block(&mut self, cpu: &mut Cpu, pc: u32) -> Option<extern "C" fn() -> u32> {
        let generation = cpu.bus.translation_generation();
        if generation != self.generation {
            // Flash, its mapping or the write guards changed: every
            // translation may be stale.
            self.slots.fill((u32::MAX, Slot::Unavailable));
            self.generation = generation;
        }
        let index = (pc >> 1) as usize & (SLOTS - 1);
        let entry = &mut self.slots[index];
        if entry.0 != pc {
            *entry = (pc, Slot::Counting(0));
        }
        match &mut entry.1 {
            Slot::Ready(function) => return Some(*function),
            Slot::Unavailable => return None,
            Slot::Counting(count) if *count < HOT => {
                *count += 1;
                return None;
            }
            Slot::Counting(_) => {}
        }
        let compiled = cpu.translate_block(pc).and_then(|block| {
            // SAFETY: the host reads `length` bytes from linear memory.
            let index = unsafe { jit_compile(block.wasm.as_ptr(), block.wasm.len()) };
            // SAFETY: a nonnegative result is the table index of a function
            // of type () -> i32; on wasm32 function pointers are table indices.
            (index > 0).then(|| unsafe {
                std::mem::transmute::<usize, extern "C" fn() -> u32>(index as usize)
            })
        });
        self.slots[index].1 = match compiled {
            Some(function) => {
                self.stats.compiled += 1;
                Slot::Ready(function)
            }
            None => {
                self.stats.failed += 1;
                Slot::Unavailable
            }
        };
        compiled
    }

    /// Execute `budget` steps, using translated blocks where possible.
    pub fn run(&mut self, cpu: &mut Cpu, budget: u64) -> Result<(), String> {
        let end = cpu.steps.saturating_add(budget);
        while cpu.steps < end {
            if !self.enabled || !cpu.batch_ready() {
                cpu.step_many(1).map_err(|error| error.to_string())?;
                continue;
            }
            let start_pc = cpu.pc;
            let (devices, polls) = cpu.batch_counters();
            let mut count = 0u64;
            let mut poll_pc = None;
            while count < BATCH && !cpu.is_idle() {
                let pc = cpu.pc;
                let block = if cpu.needs_interpreter() {
                    None
                } else {
                    self.block(cpu, pc)
                };
                let last_pc = match block {
                    Some(function) => {
                        // SAFETY: see `fm1_jit_exec`. `cpu` is not used while
                        // the block runs; it reaches the CPU only via EXEC.
                        let executed = unsafe {
                            let exec = &mut *std::ptr::addr_of_mut!(EXEC);
                            exec.cpu = cpu as *mut Cpu;
                            exec.pc = u32::MAX;
                            let executed = function();
                            exec.cpu = std::ptr::null_mut();
                            if let Some(fault) = exec.fault.take() {
                                return Err(fault);
                            }
                            executed
                        };
                        count += executed as u64;
                        self.stats.translated_steps += executed as u64;
                        self.stats.block_calls += 1;
                        // SAFETY: plain read of the shared state.
                        unsafe { (*std::ptr::addr_of!(EXEC)).pc }
                    }
                    None => {
                        let (pc, _) = cpu.interpret().map_err(|error| error.to_string())?;
                        count += 1;
                        pc
                    }
                };
                let (now_devices, now_polls) = cpu.batch_counters();
                if now_polls != polls {
                    poll_pc = Some(last_pc);
                    break;
                }
                if now_devices != devices {
                    break;
                }
            }
            self.stats.batches += 1;
            cpu.finish_batch(count, start_pc, poll_pc)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
