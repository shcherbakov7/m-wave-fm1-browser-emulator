// SPDX-License-Identifier: GPL-3.0-only
// Run a firmware for SKIP steps, then trace COUNT instructions with registers.
use fm1_emu::{cpu::Cpu, firmware::Firmware};
use std::{env, fs, path::Path};

fn main() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("usage: trace FIRMWARE SKIP [COUNT]".into());
    }
    let skip: u64 = args[1].parse().map_err(|_| "SKIP")?;
    let count: u64 = args.get(2).map_or(Ok(40), |s| s.parse()).map_err(|_| "COUNT")?;
    let firmware = Firmware::from_bytes(fs::read(Path::new(&args[0])).map_err(|e| e.to_string())?)?;
    let mut cpu = Cpu::new(firmware.bus()?, firmware.entry);
    cpu.r[0] = 0x01c7_fe08;
    for _ in 0..skip {
        cpu.step().map_err(|e| e.to_string())?;
    }
    for _ in 0..count {
        let pc = cpu.pc;
        let words: Vec<String> = (0..3)
            .map(|i| cpu.bus.read(pc + i * 2, 2).map_or("????".into(), |w| format!("{w:04x}")))
            .collect();
        let op = cpu.step().map_err(|e| e.to_string())?;
        let regs: Vec<String> = cpu.r.iter().map(|r| format!("{r:08x}")).collect();
        println!("{pc:08x} {} {op:<28} {}", words.join(" "), regs.join(" "));
    }
    Ok(())
}
