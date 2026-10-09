// SPDX-License-Identifier: GPL-3.0-only
// Write the translated WebAssembly for the region at a PC, after running a
// firmware image until it first reaches that PC.
// Usage: dumpblock FIRMWARE PC_HEX OUT.wasm [MAX_STEPS]
use fm1_emu::{cpu::Cpu, firmware::Firmware};
use std::{env, path::Path};

fn main() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    let [path, pc, out, ..] = &args[..] else {
        return Err("usage: dumpblock FIRMWARE PC_HEX OUT.wasm [MAX_STEPS]".into());
    };
    let pc = u32::from_str_radix(pc.trim_start_matches("0x"), 16).map_err(|e| e.to_string())?;
    let limit: u64 = args.get(3).map_or(Ok(2_000_000_000), |s| s.parse()).map_err(|_| "steps")?;
    let firmware = Firmware::load(Path::new(path))?;
    let mut cpu = Cpu::new(firmware.bus()?, firmware.entry);
    cpu.r[0] = 0x01c7_fe08;
    while cpu.pc != pc && cpu.steps < limit {
        cpu.step().map_err(|e| e.to_string())?;
    }
    let words: Vec<String> = (0..24)
        .map(|i| format!("{:04x}", cpu.bus.fetch(pc + 2 * i).unwrap_or(0)))
        .collect();
    println!("{pc:08x}: {}", words.join(" "));
    let block = cpu.translate_block(pc, None).ok_or("not translatable")?;
    std::fs::write(out, &block.wasm).map_err(|e| e.to_string())?;
    println!("{} instructions, {} bytes, after {} steps", block.instructions, block.wasm.len(), cpu.steps);
    Ok(())
}
