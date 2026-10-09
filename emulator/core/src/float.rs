// SPDX-License-Identifier: GPL-3.0-only
// r3 single-precision register operations, decoded with vendor objdump -mcpu=r3.
// Arithmetic/MAC rounding and conversions are checked with FM-1_986. Results
// for infinities/NaN and out-of-range conversions have no hardware capture yet;
// they follow IEEE 754 (and saturating conversion, NaN to 0) so that firmware
// which divides by zero keeps running.
pub(crate) struct FloatResult {
    pub value: u32,
    pub flags: Option<u32>,
}

pub(crate) fn result(x: u32, registers: &[u32; 16]) -> Result<Option<FloatResult>, &'static str> {
    let d = (x >> 12) as usize;
    let s = ((x >> 4) & 15) as usize;
    let c = ((x >> 8) & 15) as usize;
    let a = f32::from_bits(registers[s]);
    let b = f32::from_bits(registers[c]);
    if x & 15 == 15 {
        let value = match x & 255 {
            0x8f => (registers[c] as i32 as f32).to_bits(),
            0x9f => (registers[c] as f32).to_bits(),
            // Truncating conversions; Rust `as` saturates and maps NaN to 0.
            0x1f => (b.trunc() as i32) as u32,
            0x5f => b.trunc() as u32,
            _ => return Ok(None),
        };
        return Ok(Some(FloatResult { value, flags: None }));
    }
    if matches!(x & 15, 5 | 6) {
        let minimum = x & 15 == 5;
        let value = if a == 0.0 && b == 0.0 {
            // Hardware keeps -0 for MIN and +0 for MAX, in either order.
            if minimum {
                registers[s] | registers[c]
            } else {
                registers[s] & registers[c]
            }
        } else if minimum {
            a.min(b).to_bits()
        } else {
            a.max(b).to_bits()
        };
        let flags = ((a >= b) as u32) << 1 | ((a == b) as u32) << 2 | ((a < b) as u32) << 3;
        return Ok(Some(FloatResult {
            value,
            flags: Some(flags),
        }));
    }
    let value = match x & 15 {
        0 => a + b,
        1 => a - b,
        2 => a * b,
        3 => a / b,
        // Hardware rounds the product before accumulation; it is not fused.
        7 => f32::from_bits(registers[d]) + a * b,
        8 => f32::from_bits(registers[d]) - a * b,
        _ => return Ok(None),
    };
    Ok(Some(FloatResult {
        value: value.to_bits(),
        flags: None,
    }))
}
