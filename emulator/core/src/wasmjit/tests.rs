// SPDX-License-Identifier: GPL-3.0-only
// Differential tests: each generated instruction runs once in the interpreter
// and once as a translated block (in wasmi), from the same random state; the
// registers, PC, PSR, SRAM and write counter must agree afterwards.
use super::*;
use crate::{bus::Bus, cpu::Cpu};
use std::collections::BTreeMap;
use wasmi::{Caller, Engine, Linker, Memory, MemoryType, Module, Store};

const STATE_R: u32 = 0x100;
const STATE_SR: u32 = 0x140;
const STATE_PC: u32 = 0x180;
const STATE_IRQ: u32 = 0x184;
const STATE_WRITES: u32 = 0x188;
const STATE_RAM: u32 = 0x10000;
const STATE_XIP: u32 = 0x90000;

fn layout(cpu: &Cpu) -> Layout {
    Layout {
        r: STATE_R,
        sr: STATE_SR,
        pc: STATE_PC,
        ram: STATE_RAM,
        writes: STATE_WRITES,
        interrupts: STATE_IRQ,
        windows: cpu.bus.guard_windows(),
        xip: vec![(XIP, XIP + cpu.bus.flash.len() as u32, STATE_XIP)],
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// Interpreter-visible state copied between the CPU and wasm memory.
#[derive(Clone, PartialEq, Debug)]
struct State {
    r: [u32; 16],
    sr: [u32; 16],
    pc: u32,
    writes: u32,
    interrupts: bool,
}

fn capture(cpu: &Cpu) -> State {
    State {
        r: cpu.r,
        sr: cpu.sr,
        pc: cpu.pc,
        writes: cpu.bus.writes,
        interrupts: cpu.interrupts_enabled,
    }
}

fn apply(cpu: &mut Cpu, state: &State) {
    cpu.r = state.r;
    cpu.sr = state.sr;
    cpu.pc = state.pc;
    cpu.bus.writes = state.writes;
    cpu.interrupts_enabled = state.interrupts;
}

fn word(memory: &[u8], at: u32) -> u32 {
    u32::from_le_bytes(memory[at as usize..at as usize + 4].try_into().unwrap())
}

fn put(memory: &mut [u8], at: u32, value: u32) {
    memory[at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes());
}

fn to_wasm(memory: &mut [u8], cpu: &Cpu) {
    for i in 0..16 {
        put(memory, STATE_R + 4 * i, cpu.r[i as usize]);
        put(memory, STATE_SR + 4 * i, cpu.sr[i as usize]);
    }
    put(memory, STATE_PC, cpu.pc);
    put(memory, STATE_WRITES, cpu.bus.writes);
    memory[STATE_IRQ as usize] = cpu.interrupts_enabled as u8;
    let flash = &cpu.bus.flash;
    memory[STATE_XIP as usize..STATE_XIP as usize + flash.len()].copy_from_slice(flash);
    let ram = cpu.bus.ram();
    memory[STATE_RAM as usize..STATE_RAM as usize + ram.len()].copy_from_slice(ram);
}

fn from_wasm(memory: &[u8], cpu: &mut Cpu) {
    let mut state = capture(cpu);
    for i in 0..16 {
        state.r[i as usize] = word(memory, STATE_R + 4 * i);
        state.sr[i as usize] = word(memory, STATE_SR + 4 * i);
    }
    state.pc = word(memory, STATE_PC);
    state.writes = word(memory, STATE_WRITES);
    state.interrupts = memory[STATE_IRQ as usize] != 0;
    apply(cpu, &state);
    let len = cpu.bus.ram().len();
    cpu.bus
        .ram_mut()
        .copy_from_slice(&memory[STATE_RAM as usize..STATE_RAM as usize + len]);
}

struct Host {
    cpu: Cpu,
    memory: Option<Memory>,
    exec_calls: u32,
    fault: bool,
}

fn cpu_with(code: &[u16]) -> Cpu {
    let mut image: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    image.resize(64, 0);
    Cpu::new(Bus::new(image).unwrap(), XIP)
}

fn random_state(rng: &mut Rng, cpu: &mut Cpu) {
    let pointer = |rng: &mut Rng| RAM + 0x1000 + (rng.below(0x7c000) & !3);
    for i in 0..16 {
        cpu.r[i] = match rng.below(8) {
            0..=3 => pointer(rng),
            4 => rng.below(64),
            5 => (rng.next() as i32 >> rng.below(31)) as u32,
            6 => [0, 1, u32::MAX, 0x8000_0000, 0x7fff_ffff, 31, 32][rng.below(7) as usize],
            _ => rng.next(),
        };
    }
    for i in 0..16 {
        cpu.sr[i] = rng.next();
    }
    cpu.sr[14] = RAM + 0x40000 + (rng.below(0x1000) & !3);
    cpu.sr[3] = XIP + (rng.below(0x1000) & !1);
    cpu.sr[11] &= !0x200; // keep interrupts off; exec must not take one
    cpu.interrupts_enabled = rng.below(2) == 0;
    let ram = cpu.bus.ram_mut();
    for (i, byte) in ram.iter_mut().enumerate() {
        *byte = (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[3];
    }
}

/// Run `code` once in each engine from the same random state. Returns the
/// interpreter's op name and whether the translated code ran without `exec`.
fn compare(code: &[u16], rng: &mut Rng) -> (&'static str, bool) {
    compare_block(code, rng, 1)
}

/// Translate up to `limit` instructions; the interpreter then runs as many
/// instructions as the block reports.
fn compare_block(code: &[u16], rng: &mut Rng, limit: u32) -> (&'static str, bool) {
    compare_guarded(code, rng, limit, None)
}

/// As `compare_block`, optionally with one enabled write-protection window.
fn compare_guarded(
    code: &[u16],
    rng: &mut Rng,
    limit: u32,
    window: Option<(u32, u32)>,
) -> (&'static str, bool) {
    let mut reference = cpu_with(code);
    random_state(rng, &mut reference);
    let mut host_cpu = cpu_with(code);
    if let Some((low, high)) = window {
        for cpu in [&mut reference, &mut host_cpu] {
            cpu.bus.write(0x1eee2c0, low, 4).unwrap();
            cpu.bus.write(0x1eee280, high, 4).unwrap();
            cpu.bus.write(0x1eee348, 1, 4).unwrap();
        }
    }
    random_state(&mut Rng(0), &mut host_cpu);
    // Same state for both: copy the reference's.
    apply(&mut host_cpu, &capture(&reference));
    host_cpu
        .bus
        .ram_mut()
        .copy_from_slice(&reference.bus.ram().to_vec());
    host_cpu.sr = reference.sr;
    host_cpu.interrupts_enabled = reference.interrupts_enabled;

    let block = translate_limited(
        &reference.bus,
        &mut crate::decode::Cache::new(),
        &layout(&reference),
        XIP,
        limit,
        if limit == 1 { 1 } else { 6 },
    )
    .expect("translatable");
    wasmparser::validate(&block.wasm).expect("valid wasm");

    let before = capture(&reference);

    let engine = Engine::default();
    let mut store = Store::new(
        &engine,
        Host {
            cpu: host_cpu,
            memory: None,
            exec_calls: 0,
            fault: false,
        },
    );
    let pages = (STATE_XIP + 0x10000).div_ceil(65536) + 1;
    let memory = Memory::new(&mut store, MemoryType::new(pages, None)).unwrap();
    store.data_mut().memory = Some(memory);
    {
        let (data, host) = memory.data_and_store_mut(&mut store);
        to_wasm(data, &host.cpu);
    }
    let mut linker = Linker::<Host>::new(&engine);
    linker.define("env", "memory", memory).unwrap();
    linker
        .func_wrap("env", "exec", |mut caller: Caller<'_, Host>| -> i32 {
            let memory = caller.data().memory.unwrap();
            let (data, host) = memory.data_and_store_mut(&mut caller);
            from_wasm(data, &mut host.cpu);
            host.exec_calls += 1;
            let stop = match host.cpu.interpret_for_block() {
                Ok((_, _, stop)) => stop as i32,
                Err(_) => {
                    host.fault = true;
                    1
                }
            };
            to_wasm(data, &host.cpu);
            stop
        })
        .unwrap();
    linker
        .func_wrap(
            "env",
            "exec_pred",
            |mut caller: Caller<'_, Host>, then_end: i32, end: i32| -> i32 {
                let memory = caller.data().memory.unwrap();
                let (data, host) = memory.data_and_store_mut(&mut caller);
                from_wasm(data, &mut host.cpu);
                host.exec_calls += 1;
                if host
                    .cpu
                    .interpret_predicated(then_end as u32, end as u32)
                    .is_err()
                {
                    host.fault = true;
                }
                to_wasm(data, &host.cpu);
                1
            },
        )
        .unwrap();
    let module = Module::new(&engine, &block.wasm).unwrap();
    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let run = instance.get_typed_func::<(), i32>(&store, "b").unwrap();
    let count = run.call(&mut store, ()).unwrap();

    let mut reference_result = Ok((0, "empty"));
    for _ in 0..count.max(1) {
        reference_result = reference.execute_current();
        if reference_result.is_err() {
            break;
        }
    }
    let host = store.data();
    let name = match reference_result {
        Ok((_, name)) => name,
        Err(_) => {
            assert!(host.fault, "{code:04x?}: interpreter faulted, translation did not");
            return ("fault", false);
        }
    };
    assert!(!host.fault, "{code:04x?}: translation faulted, interpreter did not");
    assert!(count >= 1, "{code:04x?} ({name}) ran nothing");
    let mut jit = cpu_with(code);
    from_wasm(memory.data(&store), &mut jit);
    let (expected, actual) = (capture(&reference), capture(&jit));
    assert_eq!(
        expected, actual,
        "{code:04x?} ({name}) from {before:08x?}: interpreter (left) vs translation (right)"
    );
    let jit_predicate = if host.exec_calls > 0 {
        host.cpu.predicate()
    } else {
        None
    };
    assert_eq!(
        reference.predicate(),
        jit_predicate,
        "{code:04x?} ({name}): conditional-block state"
    );
    assert!(
        reference.bus.ram() == jit.bus.ram(),
        "{code:04x?} ({name}): SRAM differs"
    );
    (name, store.data().exec_calls == 0)
}

/// Random words, biased towards the wide (0xE000-0xEFFF) and common forms.
fn random_code(rng: &mut Rng) -> [u16; 3] {
    let fixed = |rng: &mut Rng| -> u32 {
        let r = rng.below(16);
        match rng.below(15) {
            0 => 0x0400,
            1 => 0x0410,
            2 => 0x0080,
            3 => 0x0460 | r,
            4 => 0x0440 | r,
            5 => 0x0470 | r.max(4),
            6 => 0x0430 | r.max(4),
            7 => 0x0450 | r.max(4),
            8 => 0x1440 | (r & 3),
            9 => 0x00c0 | r,
            10 => 0xff80,
            11 => 0xffc0 | r,
            12 => [0x0060, 0x0061][rng.below(2) as usize],
            13 => [0xe1f0, 0xe1a0 | r, 0xe1b0 | r, 0xf1c8, 0xd601, 0xc000 | rng.below(0x2000)]
                [rng.below(6) as usize],
            _ => 0xffe0 | r,
        }
    };
    let h = match rng.below(5) {
        0 => 0xe000 | rng.below(0x1000),
        1 => 0xf800 | rng.below(0x800),
        2 => fixed(rng),
        _ => rng.below(0x10000),
    } as u16;
    [h, rng.next() as u16, rng.next() as u16]
}

#[test]
fn random_instructions_match_the_interpreter() {
    let mut rng = Rng(0x5eed_1234_abcd);
    let mut translated: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut interpreted: BTreeMap<&'static str, u32> = BTreeMap::new();
    for _ in 0..80_000 {
        let code = random_code(&mut rng);
        let (name, direct) = compare(&code, &mut rng);
        *if direct { &mut translated } else { &mut interpreted }
            .entry(name)
            .or_default() += 1;
    }
    // The hot forms must actually be translated, not just delegated.
    for name in [
        "mov_imm32",
        "mov_imm8",
        "mov_reg",
        "add",
        "sub",
        "add_imm8",
        "load32",
        "store32",
        "stack_word",
        "branch_zero",
        "branch_compare_immediate",
        "branch_compare_register",
        "call_rel22",
        "return",
        "push_rets",
        "pop_pc",
        "memory_add",
        "byte_extended",
        "word_extended",
        "halfword_extended",
        "memory_indexed",
        "add_immediate",
        "logic_immediate",
        "shift_extended",
        "sti",
        "cli",
        "conditional_block",
        "multiply_extended",
        "bit_field",
    ] {
        assert!(
            translated.get(name).copied().unwrap_or(0) > 0,
            "{name} never translated; translated: {translated:?}"
        );
    }
    eprintln!("translated: {translated:?}");
    eprintln!("interpreted: {interpreted:?}");
}

#[test]
fn random_sequences_match_the_interpreter() {
    let mut rng = Rng(0xfeed_beef_0042);
    for _ in 0..8_000 {
        let mut code = Vec::new();
        for _ in 0..8 {
            code.extend_from_slice(&random_code(&mut rng));
        }
        compare_block(&code, &mut rng, 8);
    }
}

#[test]
fn stores_into_a_write_protection_window_fault_like_the_interpreter() {
    let mut rng = Rng(0x0bad_cafe_77);
    for _ in 0..6_000 {
        let code = random_code(&mut rng);
        // A window around the pointers random_state hands out.
        let low = RAM + 0x1000 + (rng.below(0x7c000) & !3);
        let high = low + rng.below(0x8000);
        compare_guarded(&code, &mut rng, 1, Some((low, high)));
    }
}
