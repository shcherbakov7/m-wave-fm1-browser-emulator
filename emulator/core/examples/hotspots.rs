// SPDX-License-Identifier: GPL-3.0-only
// Sample where guest time goes: run a firmware image, then histogram the PC
// (and the core) every few steps over a window, with nearby instructions.
// Usage: hotspots FIRMWARE [WARMUP_STEPS] [WINDOW_STEPS]
use fm1_emu::{cpu::Cpu, firmware::Firmware};
use std::{collections::HashMap, env, path::Path};

fn main() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    let path = args.first().ok_or("usage: hotspots FIRMWARE [WARMUP] [WINDOW]")?;
    let number = |i: usize, default: u64| args.get(i).map_or(Ok(default), |s| s.parse()).map_err(|_| "steps");
    let (warmup, window) = (number(1, 400_000_000)?, number(2, 200_000_000)?);
    let firmware = Firmware::load(Path::new(path))?;
    let mut cpu = Cpu::new(firmware.bus()?, firmware.entry);
    cpu.r[0] = 0x01c7_fe08;
    cpu.step_many(warmup).map_err(|e| e.to_string())?;
    let (steps, ticks, batched) = (cpu.steps, cpu.bus.oscillator_ticks, cpu.batched_steps);
    let started = std::time::Instant::now();
    let mut pcs: HashMap<u32, u64> = HashMap::new();
    let mut samples = 0u64;
    let (mut code_changes, mut translation_changes) = (0, 0);
    let (mut code, mut translation) = (cpu.bus.code_generation(), cpu.bus.translation_generation());
    while cpu.steps < steps + window {
        cpu.step_many(61).map_err(|e| e.to_string())?;
        code_changes += (cpu.bus.code_generation() != code) as u32;
        translation_changes += (cpu.bus.translation_generation() != translation) as u32;
        (code, translation) = (cpu.bus.code_generation(), cpu.bus.translation_generation());
        *pcs.entry(if env::var_os("EXACT").is_some() { cpu.pc } else { cpu.pc & !0xf }).or_default() += 1;
        samples += 1;
    }
    let guest = (cpu.bus.oscillator_ticks - ticks) as f64 / 24e6;
    println!("code generation changed {code_changes}x, translation generation {translation_changes}x");
    println!(
        "{:.1} M steps/s natively, {:.1}% batched",
        (cpu.steps - steps) as f64 / started.elapsed().as_secs_f64() / 1e6,
        (cpu.batched_steps - batched) as f64 * 100.0 / (cpu.steps - steps) as f64
    );
    println!("{} steps in {guest:.3} guest s = {:.1} M instr per guest s", cpu.steps - steps, (cpu.steps - steps) as f64 / guest / 1e6);
    let mut hot: Vec<_> = pcs.into_iter().collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1));
    for (pc, count) in hot.into_iter().take(40) {
        println!("{:5.1}% {pc:08x}", count as f64 * 100.0 / samples as f64);
    }
    if env::var_os("EXACT").is_some() {
        println!(
            "secondary (pc, idle, locked) {:x?}; primary idle {} batch_ready {} control {:x} {:x}",
            cpu.secondary_state(),
            cpu.is_idle(),
            cpu.batch_ready(),
            cpu.bus.core_control(0),
            cpu.bus.core_control(1)
        );
        for _ in 0..12 {
            let pc = cpu.pc;
            let op = cpu.step().map_err(|e| e.to_string())?;
            let regs: Vec<String> = cpu.r.iter().map(|v| format!("{v:08x}")).collect();
            println!("{pc:08x} {op:24} {} psr={:08x}", regs.join(" "), cpu.sr[5]);
        }
    }
    Ok(())
}
