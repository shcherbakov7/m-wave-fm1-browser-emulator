// SPDX-License-Identifier: GPL-3.0-only
//! Dispatcher for translated blocks (WebAssembly host only).
//!
//! Hot XIP blocks are translated by `fm1_emu::wasmjit`, compiled by the
//! JavaScript host (`env.jit_compile`, which instantiates the module and
//! appends its function to this module's indirect function table) and then
//! called through that table. `Jit` is a `BlockRunner`, so batching (devices,
//! guest time and interrupts updated once per batch) is `Cpu::step_many_with`.
use fm1_emu::cpu::fast::{BlockRunner, PreparedRunner};
use fm1_emu::cpu::{Cpu, Fault};
use std::collections::HashMap;

#[link(wasm_import_module = "env")]
extern "C" {
    /// Compile a block module; returns its function's table index or -1.
    fn jit_compile(bytes: *const u8, length: usize) -> i32;
    /// The function at this table index is no longer used.
    fn jit_release(index: i32);
}

/// Executions before a block is translated.
const HOT: u32 = 16;

#[derive(Clone, Copy)]
enum Slot {
    Counting(u32),
    Ready(extern "C" fn(u32) -> u32),
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
}

/// Interpreter calls made by translated blocks.
pub static mut EXEC_CALLS: u64 = 0;
/// When profiling, interpreter calls from blocks by operation name and PC.
pub static mut EXEC_OPS: Option<HashMap<(&'static str, u32), u64>> = None;

pub struct Jit {
    slots: Vec<(u32, Slot)>,
    /// Chain table for translated code: `[pc, table index]` per slot, so a
    /// block can go straight on to the next one (see `Layout::chain`).
    chain: Vec<[u32; 2]>,
    /// Whether the host supports tail calls, which chaining needs.
    pub chaining: bool,
    /// Flash state the XIP translations match (`translation_generation`).
    generation: u32,
    /// SRAM code version the SRAM translations match.
    sram_generation: u32,
    /// XIP is off: chain entries into XIP translations are cleared.
    xip_chain_off: bool,
    pub enabled: bool,
    /// Check every block without interpreter calls against the interpreter
    /// (the first few calls at each entry PC; slow, for diagnostics). A
    /// mismatch stops emulation with a report.
    pub verify: bool,
    pub verified: u64,
    /// Verified calls per block entry PC (each is checked a few times).
    checks: HashMap<u32, u32>,
    pub stats: Stats,
}

impl BlockRunner for Jit {
    fn run(&mut self, cpu: &mut Cpu) -> Option<Result<(u64, u32), Fault>> {
        let pc = cpu.pc;
        let Some(function) = self.block(cpu, pc) else {
            return PreparedRunner.run(cpu);
        };
        let result = if self.verify && self.sample(pc) {
            self.verify_block(cpu, pc, function)
        } else {
            Self::call(cpu, function)
        };
        Some(match result {
            Ok(executed) => {
                self.stats.translated_steps += executed as u64;
                self.stats.block_calls += 1;
                // SAFETY: plain read of the shared state.
                Ok((executed as u64, unsafe { (*std::ptr::addr_of!(EXEC)).pc }))
            }
            Err(message) => Err(Fault::Host(message)),
        })
    }
}

impl Drop for Jit {
    fn drop(&mut self) {
        self.release_all();
    }
}

impl Jit {
    /// Drop every translation and hand its function back to the host.
    fn release_all(&mut self) {
        self.release_where(|_| true);
    }

    /// Drop the translations of blocks starting at PCs matching `stale`.
    fn release_where(&mut self, stale: impl Fn(u32) -> bool) {
        for (index, (pc, slot)) in self.slots.iter_mut().enumerate() {
            if *pc == u32::MAX || !stale(*pc) {
                continue;
            }
            if let Slot::Ready(function) = slot {
                // SAFETY: host import; nothing calls the index any more.
                unsafe { jit_release(*function as usize as i32) };
            }
            *pc = u32::MAX;
            *slot = Slot::Unavailable;
            self.chain[index] = [u32::MAX, 0];
        }
    }

    /// Drop translations that the code in flash or SRAM no longer matches.
    /// While XIP is off its translations are kept for when the same
    /// mapping returns, but nothing chains into them meanwhile.
    fn validate(&mut self, cpu: &Cpu) {
        let xip = |pc: u32| (fm1_emu::XIP..fm1_emu::XIP_END).contains(&pc);
        let sram = cpu.bus.sram_code_generation();
        if sram != self.sram_generation {
            self.release_where(|pc| !xip(pc));
            self.sram_generation = sram;
        }
        let generation = cpu.bus.translation_generation();
        if generation == self.generation {
            self.xip_chain_off = false;
        } else if cpu.bus.xip_active() {
            self.release_where(xip);
            self.generation = generation;
            self.xip_chain_off = false;
        } else if !self.xip_chain_off {
            for (index, (pc, _)) in self.slots.iter().enumerate() {
                if xip(*pc) {
                    self.chain[index] = [u32::MAX, 0];
                }
            }
            self.xip_chain_off = true;
        }
    }

    pub fn new() -> Self {
        Self {
            slots: vec![(u32::MAX, Slot::Unavailable); SLOTS],
            chain: vec![[u32::MAX, 0]; SLOTS],
            chaining: false,
            generation: u32::MAX,
            sram_generation: u32::MAX,
            xip_chain_off: false,
            enabled: true,
            verify: false,
            verified: 0,
            checks: HashMap::new(),
            stats: Stats::default(),
        }
    }

    fn block(&mut self, cpu: &mut Cpu, pc: u32) -> Option<extern "C" fn(u32) -> u32> {
        self.validate(cpu);
        let xip = (fm1_emu::XIP..fm1_emu::XIP_END).contains(&pc);
        if xip && self.xip_chain_off {
            return None; // XIP is off: let the interpreter fault
        }
        let index = (pc >> 1) as usize & (SLOTS - 1);
        let entry = &mut self.slots[index];
        if entry.0 != pc {
            if let Slot::Ready(function) = entry.1 {
                // A colliding block takes the slot (and its chain entry).
                self.chain[index] = [u32::MAX, 0];
                // SAFETY: host import; nothing refers to the index any more.
                unsafe { jit_release(function as usize as i32) };
            }
            *entry = (pc, Slot::Counting(0));
        }
        match &mut entry.1 {
            Slot::Ready(function) => {
                // Its chain entry may have been cleared while XIP was off.
                self.chain[index] = [pc, *function as usize as u32];
                return Some(*function);
            }
            Slot::Unavailable => return None,
            Slot::Counting(count) if *count < HOT => {
                *count += 1;
                return None;
            }
            Slot::Counting(_) => {}
        }
        let chain = self
            .chaining
            .then(|| (self.chain.as_ptr() as usize as u32, SLOTS as u32 - 1));
        let compiled = cpu.translate_block(pc, chain).and_then(|block| {
            // SAFETY: the host reads `length` bytes from linear memory.
            let index = unsafe { jit_compile(block.wasm.as_ptr(), block.wasm.len()) };
            // SAFETY: a nonnegative result is the table index of a function
            // of type (i32) -> i32; on wasm32 function pointers are table
            // indices.
            (index > 0).then(|| unsafe {
                std::mem::transmute::<usize, extern "C" fn(u32) -> u32>(index as usize)
            })
        });
        self.slots[index].1 = match compiled {
            Some(function) => {
                self.stats.compiled += 1;
                self.chain[index] = [pc, function as usize as u32];
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
        if !self.enabled {
            return cpu.step_many(budget).map_err(|error| error.to_string());
        }
        cpu.step_many_with(budget, self).map_err(|error| error.to_string())
    }

    /// Whether to verify this call: the first few calls entering at `pc`.
    fn sample(&mut self, pc: u32) -> bool {
        let checks = self.checks.entry(pc).or_default();
        *checks += 1;
        *checks <= 8
    }

    fn call(cpu: &mut Cpu, function: extern "C" fn(u32) -> u32) -> Result<u32, String> {
        // SAFETY: as in `run`.
        unsafe {
            let exec = &mut *std::ptr::addr_of_mut!(EXEC);
            exec.cpu = cpu as *mut Cpu;
            exec.pc = u32::MAX;
            let executed = function(0);
            exec.cpu = std::ptr::null_mut();
            if let Some(fault) = exec.fault.take() {
                return Err(fault);
            }
            Ok(executed)
        }
    }

    /// Run a block, then rerun the same instructions in the interpreter from
    /// the saved state and require identical registers and SRAM.
    fn verify_block(
        &mut self,
        cpu: &mut Cpu,
        pc: u32,
        function: extern "C" fn(u32) -> u32,
    ) -> Result<u32, String> {
        let before = cpu.block_state();
        let ram_before = cpu.bus.ram().to_vec();
        // SAFETY: plain reads on the single host thread.
        let calls = unsafe { *std::ptr::addr_of!(EXEC_CALLS) };
        let executed = Self::call(cpu, function)?;
        if unsafe { *std::ptr::addr_of!(EXEC_CALLS) } != calls {
            return Ok(executed); // devices may have been touched: not repeatable
        }
        let translated = cpu.block_state();
        let ram_translated = cpu.bus.ram().to_vec();
        cpu.set_block_state(before);
        cpu.bus.ram_mut().copy_from_slice(&ram_before);
        for _ in 0..executed {
            cpu.interpret().map_err(|error| error.to_string())?;
        }
        let interpreted = cpu.block_state();
        let mut differences = Vec::new();
        for i in 0..16 {
            if translated.0[i] != interpreted.0[i] {
                differences.push(format!("r{i} {:08x}≠{:08x}", translated.0[i], interpreted.0[i]));
            }
            if translated.1[i] != interpreted.1[i] {
                differences.push(format!("sr{i} {:08x}≠{:08x}", translated.1[i], interpreted.1[i]));
            }
        }
        if translated.2 != interpreted.2 {
            differences.push(format!("pc {:08x}≠{:08x}", translated.2, interpreted.2));
        }
        if translated.3 != interpreted.3 {
            differences.push("interrupt enable".into());
        }
        if translated.4 != interpreted.4 {
            differences.push(format!("writes {}≠{}", translated.4, interpreted.4));
        }
        let ram = cpu.bus.ram();
        if let Some(offset) = (0..ram.len()).find(|&i| ram[i] != ram_translated[i]) {
            differences.push(format!(
                "SRAM {:08x}: {:02x}≠{:02x}",
                fm1_emu::RAM + offset as u32,
                ram_translated[offset],
                ram[offset]
            ));
        }
        self.verified += 1;
        if differences.is_empty() {
            return Ok(executed);
        }
        let regs: Vec<String> = before.0.iter().map(|v| format!("{v:08x}")).collect();
        Err(format!(
            "JIT mismatch in block {pc:08x} after {executed} instructions (translated≠interpreted): {}; \
             entry r=[{}] sr5={:08x} sr14={:08x}",
            differences.join(", "),
            regs.join(" "),
            before.1[5],
            before.1[14]
        ))
    }
}
