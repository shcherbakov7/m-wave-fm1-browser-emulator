// SPDX-License-Identifier: GPL-3.0-only
// Instruction forms added for Felucca 1.1.x, Melodee, X0X and SLOOP builds.
// Encodings and meanings come from the vendor objdump (`analysis/` in
// AL-255/FM-1-RE) unless a comment says otherwise.
use fm1_emu::{bus::Bus, cpu::Cpu, RAM, XIP};

fn cpu(words: &[u16]) -> Cpu {
    Cpu::new(
        Bus::new(words.iter().flat_map(|w| w.to_le_bytes()).collect()).unwrap(),
        XIP,
    )
}

const DATA: u32 = RAM + 0x1000;

#[test]
fn halfword_register_preincrement_store() {
    // EDDC 0B31 (kind 1): h[++r3=r11] = r0, writing back the base first.
    let mut c = cpu(&[0xeddc, 0x0b31]);
    c.r[3] = DATA;
    c.r[11] = 6;
    c.r[0] = 0xdead_beef;
    c.step().unwrap();
    assert_eq!(c.r[3], DATA + 6);
    assert_eq!(c.bus.read(DATA + 6, 2).unwrap(), 0xbeef);
    assert_eq!(c.pc, XIP + 4);
}

#[test]
fn word_postincrement_carries_the_signed_high_stride_in_the_opcode() {
    // ECDA 0014: r0 = [r1++=516].
    let mut c = cpu(&[0xecda, 0x0014]);
    c.r[1] = DATA;
    c.bus.write(DATA, 0x1234_5678, 4).unwrap();
    c.step().unwrap();
    assert_eq!(c.r[0], 0x1234_5678);
    assert_eq!(c.r[1], DATA + 516);
    // ECDF 0F0D (-4 stride, store): [r0++=-4] = r0's partner r0.
    let mut c = cpu(&[0xecdf, 0xff1d]);
    c.r[1] = DATA + 8;
    c.r[15] = 7;
    c.step().unwrap();
    assert_eq!(c.bus.read(DATA + 8, 4).unwrap(), 7);
    assert_eq!(c.r[1], DATA + 4);
}

#[test]
fn halfword_postincrement_uses_a_signed_ten_bit_stride() {
    // EDD3 0D3F: h[r0++=-4] = r3.
    let mut c = cpu(&[0xedd3, 0x3f0d]);
    c.r[0] = DATA + 8;
    c.r[3] = 0xabcd;
    c.step().unwrap();
    assert_eq!(c.bus.read(DATA + 8, 2).unwrap(), 0xabcd);
    assert_eq!(c.r[0], DATA + 4);
    // EDD3 CF5E: r12 = h[r5++=-2] (u).
    let mut c = cpu(&[0xedd3, 0xcf5e]);
    c.r[5] = DATA;
    c.bus.write(DATA, 0x8001, 2).unwrap();
    c.step().unwrap();
    assert_eq!(c.r[12], 0x8001);
    assert_eq!(c.r[5], DATA - 2);
}

#[test]
fn register_pair_postincrement_indexed_and_preincrement() {
    let seed = |c: &mut Cpu| {
        c.bus.write(DATA + 16, 0x1111_1111, 4).unwrap();
        c.bus.write(DATA + 20, 0x2222_2222, 4).unwrap();
    };
    // EC58 2008: r3_r2 = d[r0++=8].
    let mut c = cpu(&[0xec58, 0x2008]);
    seed(&mut c);
    c.r[0] = DATA + 16;
    c.step().unwrap();
    assert_eq!((c.r[2], c.r[3], c.r[0]), (0x1111_1111, 0x2222_2222, DATA + 24));
    // EC58 040A: r1_r0 = d[r0+r4<<3].
    let mut c = cpu(&[0xec58, 0x040a]);
    seed(&mut c);
    c.r[0] = DATA;
    c.r[4] = 2;
    c.step().unwrap();
    assert_eq!((c.r[0], c.r[1]), (0x1111_1111, 0x2222_2222));
    // EC5C 4012: r5_r4 = d[++r1=r0].
    let mut c = cpu(&[0xec5c, 0x4012]);
    seed(&mut c);
    c.r[1] = DATA + 4;
    c.r[0] = 12;
    c.step().unwrap();
    assert_eq!((c.r[4], c.r[5], c.r[1]), (0x1111_1111, 0x2222_2222, DATA + 16));
    // EC5C 4013: d[++r1=r0] = r5_r4 (the X0X form).
    let mut c = cpu(&[0xec5c, 0x4013]);
    c.r[1] = DATA;
    c.r[0] = 8;
    c.r[4] = 0xaaaa_aaaa;
    c.r[5] = 0xbbbb_bbbb;
    c.step().unwrap();
    assert_eq!(c.bus.read(DATA + 8, 4).unwrap(), 0xaaaa_aaaa);
    assert_eq!(c.bus.read(DATA + 12, 4).unwrap(), 0xbbbb_bbbb);
    assert_eq!(c.r[1], DATA + 8);
}

#[test]
fn unsigned_greater_than_packed_conditional_block() {
    // EC20: kind 0xC2 = if (r0 > #packed) {…}, by the kind-field pattern
    // (operand 1/2/3 = register/packed/literal, bit 3 negates). ECA0 0F02
    // is the vendor's packed 520, so EC20 1F02 selects r0 > 520.
    for value in [0u32, 519, 520, 521, u32::MAX] {
        let mut c = cpu(&[0xec20, 0x1f02, 0x2b42, 0x3642, 0]);
        c.r[0] = value;
        for _ in 0..3 {
            c.step().unwrap();
        }
        assert_eq!(c.r[2], if value > 520 { 11 } else { 22 }, "r0={value}");
        assert_eq!(c.pc, XIP + 10);
    }
}

#[test]
fn float_branches_cover_the_unsigned_opcode_family() {
    // E901 0802: operand bit 11 makes `if (r0 >= r1) goto +4` an f32 compare.
    for (lhs, rhs, taken) in [
        (0xbf80_0000u32, 0x3f80_0000u32, false), // -1.0 >= 1.0
        (0x3f80_0000, 0xbf80_0000, true),
        (0x3f80_0000, 0x3f80_0000, true),
    ] {
        let mut c = cpu(&[0xe901, 0x0802]);
        c.r[0] = lhs;
        c.r[1] = rhs;
        c.step().unwrap();
        assert_eq!(c.pc, XIP + if taken { 8 } else { 4 });
    }
}

#[test]
fn float_division_by_zero_follows_ieee() {
    // E53F 64B3: r6 = r11 / r4. X0X runs the parallel-prefixed F53F form of
    // this at 0x02020f8c and divides 1.0 by 0.0.
    let mut c = cpu(&[0xe53f, 0x64b3]);
    c.r[11] = 1.0f32.to_bits();
    c.r[4] = 0;
    c.step().unwrap();
    assert_eq!(f32::from_bits(c.r[6]), f32::INFINITY);
}

#[test]
fn unsigned_immediate_branches_zero_extend_and_equality_sign_extends() {
    // Vendor F940 ACBC: if (r0 >= 598) goto ... (unsigned literal). SLOOP's
    // TIMER4 delay F9F1 81FB is if (r1 < 960); sign extension would loop
    // forever. Vendor F846 820F: if (r6 == -447) keeps a signed literal.
    let branch = |h: u16, x: u16, reg: usize, value: u32| {
        let mut c = cpu(&[h, x]);
        c.r[reg] = value;
        c.step().unwrap();
        c.pc != XIP + 4
    };
    assert!(branch(0xf940, 0xacbc, 0, 598));
    assert!(!branch(0xf940, 0xacbc, 0, 597));
    assert!(branch(0xf9f1, 0x81fb, 1, 959));
    assert!(!branch(0xf9f1, 0x81fb, 1, 960));
    assert!(branch(0xf846, 0x820f, 6, (-447i32) as u32));
    assert!(!branch(0xf846, 0x820f, 6, 577));
}
