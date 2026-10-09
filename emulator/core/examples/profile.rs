// SPDX-License-Identifier: GPL-3.0-only
// Instruction-mix and throughput profile for a firmware image.
use fm1_emu::{cpu::Cpu, firmware::Firmware};
use std::{collections::HashMap, env, path::Path, time::Instant};

fn main() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    let path = args.first().ok_or("usage: profile FIRMWARE [STEPS]")?;
    let limit: u64 = args.get(1).map_or(Ok(100_000_000), |s| s.parse()).map_err(|_| "steps")?;
    let firmware = Firmware::load(Path::new(path))?;
    let mut cpu = Cpu::new(firmware.bus()?, firmware.entry);
    cpu.r[0] = 0x01c7_fe08;
    // Counting ops costs a hash per step; enable it with PROFILE_OPS=1.
    let count_ops = env::var_os("PROFILE_OPS").is_some();
    let mut ops: HashMap<&'static str, u64> = HashMap::new();
    let start = Instant::now();
    let mut fault = None;
    if count_ops {
        for _ in 0..limit {
            match cpu.step() {
                Ok(op) => *ops.entry(op).or_default() += 1,
                Err(error) => {
                    fault = Some(error.to_string());
                    break;
                }
            }
        }
    } else {
        if env::var_os("PROFILE_UNBATCHED").is_some() {
            cpu.unbatched_ops = Some(HashMap::new());
        }
        if let Err(error) = cpu.step_many(limit) {
            fault = Some(error.to_string());
        }
        if let Some(unbatched) = cpu.unbatched_ops.take() {
            ops = unbatched;
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let guest = cpu.bus.audio.frames as f64 / 44_100.0;
    println!(
        "{:.1}M steps in {elapsed:.1}s = {:.1}M/s; guest audio {guest:.3}s; realtime x{:.3}",
        cpu.steps as f64 / 1e6,
        cpu.steps as f64 / 1e6 / elapsed,
        guest / elapsed
    );
    println!("code generation: {}", cpu.bus.code_generation());
    println!("batched: {:.1}%", cpu.batched_steps as f64 * 100.0 / cpu.steps.max(1) as f64);
    let mut sorted: Vec<_> = ops.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (op, count) in sorted.iter().take(25) {
        println!("{:>8.3}% {op}", *count as f64 * 100.0 / limit as f64);
    }
    if let Some(fault) = fault {
        println!("fault: {fault}");
    }
    Ok(())
}
