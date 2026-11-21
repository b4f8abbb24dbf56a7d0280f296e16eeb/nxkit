//! Tests of the executor of plain loads and stores.

use super::*;

#[test]
fn loads_extend_and_fill_registers() {
    let mut r = Rig::new();
    let w = r.word(16, 8);
    r.run(0xf940_0841, 0); // ldr x1, [x2, #16]
    assert_eq!(r.x[1], w);
    r.run(0xb940_0c41, 0); // ldr w1, [x2, #12]
    assert_eq!(r.x[1], r.word(12, 4));
    r.x[3] = u64::MAX;
    r.run(0x3940_1483, 0); // ldrb w3, [x4, #5]
    assert_eq!(r.x[3], r.word(5, 1));
    r.run(0x7940_0c83, 0); // ldrh w3, [x4, #6]
    assert_eq!(r.x[3], r.word(6, 2));
    r.mem[5] = 0x80;
    r.mem[6] = 0x00;
    r.mem[7] = 0x80;
    r.run(0x3980_1483, 0); // ldrsb x3, [x4, #5]
    assert_eq!(r.x[3], 0xffff_ffff_ffff_ff80);
    r.run(0x39c0_1483, 0); // ldrsb w3, [x4, #5]
    assert_eq!(r.x[3], 0x0000_0000_ffff_ff80);
    r.run(0x7980_0c83, 0); // ldrsh x3, [x4, #6]
    assert_eq!(r.x[3], 0xffff_ffff_ffff_8000 | u64::from(r.mem[6]));
    r.mem[8..12].copy_from_slice(&0x8000_0001u32.to_le_bytes());
    r.run(0xb980_0883, 0); // ldrsw x3, [x4, #8]
    assert_eq!(r.x[3], 0xffff_ffff_8000_0001);
    r.x[1] = 7;
    r.run(0xf940_0841 | 31, 0); // ldr xzr, [x2, #16]: no register changes
    assert_eq!(r.x[1], 7);
}

#[test]
fn stores_write_the_low_bytes_and_xzr_stores_zero() {
    let mut r = Rig::new();
    r.x[1] = 0x0102_0304_0506_0708;
    r.run(0xf900_0841, 0); // str x1, [x2, #16]
    assert_eq!(r.word(16, 8), 0x0102_0304_0506_0708);
    r.run(0xb900_0c41, 0); // str w1, [x2, #12]
    assert_eq!(r.word(12, 4), 0x0506_0708);
    r.x[3] = 0x1ff;
    r.run(0x3900_1483, 0); // strb w3, [x4, #5]
    assert_eq!(r.mem[5], 0xff);
    assert_eq!(r.mem[6], 6 ^ 0x5a, "the next byte is not touched");
    r.run(0xf900_0c5f, 0); // str xzr, [x2, #24]
    assert_eq!(r.word(24, 8), 0);
    r.x[3] = 0xaabb_ccdd_eeff_1122;
    r.run(0x7900_0c83, 0); // strh w3, [x4, #6]
    assert_eq!(r.word(6, 2), 0x1122);
}

#[test]
fn the_base_register_is_written_back_for_pre_and_post_index() {
    let mut r = Rig::new();
    let base = r.mem.as_ptr() as u64;
    let p = r.run(0xf841_0c41, 8); // ldr x1, [x2, #16]!
    assert_eq!(p.ea, base + 24);
    assert_eq!(r.x[2], base + 24);
    assert_eq!(r.x[1], r.word(24, 8));
    let p = r.run(0xf841_0441, 8); // ldr x1, [x2], #16
    assert_eq!(p.ea, base + 8);
    assert_eq!(r.x[2], base + 24);
    assert_eq!(r.x[1], r.word(8, 8));
    let p = r.run(0x29bf_0be1, 40); // stp w1, w2, [sp, #-8]!
    assert_eq!(p.ea, base + 32);
    assert_eq!(r.sp, base + 32);
    let (a, b) = (r.x[1] as u32, r.x[2] as u32);
    assert_eq!(r.word(32, 4), u64::from(a));
    assert_eq!(r.word(36, 4), u64::from(b));
}

#[test]
fn register_offsets_extend_and_shift() {
    let mut r = Rig::new();
    let base = r.mem.as_ptr() as u64;
    // ldr x1, [x2, x3, lsl #3]
    r.x[3] = 4;
    let op = d(0xf863_7841);
    r.x[2] = base;
    let rg = r.regs();
    // SAFETY: the rig's own register file.
    let p = unsafe { plan(&op, &rg) };
    assert_eq!(p.ea, base + 32);
    // ldr w1, [x2, w3, uxtw #2] with the upper half of x3 set: it is ignored.
    r.x[3] = 0xffff_ffff_0000_0003;
    let op = d(0xb863_5841);
    let rg = r.regs();
    // SAFETY: as above.
    let p = unsafe { plan(&op, &rg) };
    assert_eq!(p.ea, base + 12);
    // ldr w1, [x2, w3, sxtw]
    r.x[3] = 0x0000_0000_ffff_fffc;
    r.x[2] = base + 16;
    let op = d(0xb863_c841);
    let rg = r.regs();
    // SAFETY: as above.
    let p = unsafe { plan(&op, &rg) };
    assert_eq!(p.ea, base + 12);
    // ldr x1, [x2, x3, sxtx #3]
    r.x[3] = (-2i64) as u64;
    r.x[2] = base + 32;
    let op = d(0xf863_f841);
    let rg = r.regs();
    // SAFETY: as above.
    let p = unsafe { plan(&op, &rg) };
    assert_eq!(p.ea, base + 16);
    // A zero register as offset adds nothing: ldr x1, [x2, xzr]
    let op = d(0xf87f_6841);
    let rg = r.regs();
    // SAFETY: as above.
    let p = unsafe { plan(&op, &rg) };
    assert_eq!(p.ea, base + 32);
}

#[test]
fn pairs_load_and_store_both_registers() {
    let mut r = Rig::new();
    r.run(0xa941_0861, 0); // ldp x1, x2, [x3, #16]
    assert_eq!((r.x[1], r.x[2]), (r.word(16, 8), r.word(24, 8)));
    r.run(0x2941_0861, 0); // ldp w1, w2, [x3, #8]
    assert_eq!((r.x[1], r.x[2]), (r.word(8, 4), r.word(12, 4)));
    r.mem[8..12].copy_from_slice(&0x8000_0000u32.to_le_bytes());
    r.run(0x6941_0861, 0); // ldpsw x1, x2, [x3, #8]
    assert_eq!(r.x[1], 0xffff_ffff_8000_0000);
    assert_eq!(
        r.x[2],
        r.word(12, 4)
            | if r.mem[15] & 0x80 != 0 {
                0xffff_ffff_0000_0000
            } else {
                0
            }
    );
    r.x[1] = 11;
    r.x[2] = 22;
    r.run(0xa901_0861, 0); // stp x1, x2, [x3, #16]
    assert_eq!((r.word(16, 8), r.word(24, 8)), (11, 22));
    r.run(0xa900_fc7f, 0); // stp xzr, xzr, [x3, #8]
    assert_eq!((r.word(8, 8), r.word(16, 8)), (0, 0));
}

#[test]
fn simd_registers_move_whole_and_zero_extend() {
    let mut r = Rig::new();
    r.run(0x3dc0_0841, 0); // ldr q1, [x2, #32]
    let mut q = [0u8; 16];
    q.copy_from_slice(&r.mem[32..48]);
    assert_eq!(r.v[1], u128::from_le_bytes(q));
    r.run(0xfd40_0841, 0); // ldr d1, [x2, #16]
    assert_eq!(r.v[1], u128::from(r.word(16, 8)));
    r.run(0xbd40_0c41, 0); // ldr s1, [x2, #12]
    assert_eq!(r.v[1], u128::from(r.word(12, 4)));
    r.run(0x3d40_0c41, 0); // ldr b1, [x2, #3]
    assert_eq!(r.v[1], u128::from(r.mem[3]));
    r.run(0x7d40_0c41, 0); // ldr h1, [x2, #6]
    assert_eq!(r.v[1], u128::from(r.word(6, 2)));
    r.v[1] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    r.run(0x3d80_0841, 0); // str q1, [x2, #32]
    assert_eq!(&r.mem[32..48], &r.v[1].to_le_bytes());
    r.run(0xfd00_0841, 0); // str d1, [x2, #16]
    assert_eq!(r.word(16, 8), 0x0706_0504_0302_0100);
    r.run(0xbd00_0c41, 0); // str s1, [x2, #12]
    assert_eq!(r.word(12, 4), 0x0302_0100);
    r.run(0x7d00_0c41, 0); // str h1, [x2, #6]
    assert_eq!(r.word(6, 2), 0x0100);
    r.run(0x3d00_0c41, 0); // str b1, [x2, #3]
    assert_eq!(r.mem[3], 0);
    r.v[2] = 0x1f1e_1d1c_1b1a_1918_1716_1514_1312_1110;
    r.run(0xad00_0861, 0); // stp q1, q2, [x3]
    assert_eq!(&r.mem[0..16], &r.v[1].to_le_bytes());
    assert_eq!(&r.mem[16..32], &r.v[2].to_le_bytes());
    r.run(0xad41_0861, 0); // ldp q1, q2, [x3, #32]
    assert_eq!(
        r.v[1],
        u128::from_le_bytes(r.mem[32..48].try_into().unwrap())
    );
    assert_eq!(
        r.v[2],
        u128::from_le_bytes(r.mem[48..64].try_into().unwrap())
    );
    r.run(0x6d41_0861, 0); // ldp d1, d2, [x3, #16]
    assert_eq!(
        (r.v[1], r.v[2]),
        (u128::from(r.word(16, 8)), u128::from(r.word(24, 8)))
    );
}

#[test]
fn misaligned_accesses_copy_bytes() {
    let mut r = Rig::new();
    r.run(0xf940_0041, 1); // ldr x1, [x2] from an odd address
    assert_eq!(r.x[1], r.word(1, 8));
    r.x[1] = 0x1122_3344_5566_7788;
    r.run(0xf900_0041, 3); // str x1, [x2] to an odd address
    assert_eq!(r.word(3, 8), 0x1122_3344_5566_7788);
}

#[test]
fn ordered_loads_and_stores_use_the_register_as_the_address() {
    let mut r = Rig::new();
    r.run(0xc8df_fc41, 8); // ldar x1, [x2]
    assert_eq!(r.x[1], r.word(8, 8));
    r.run(0x08df_fc41, 9); // ldarb w1, [x2]
    assert_eq!(r.x[1], r.word(9, 1));
    r.x[1] = 0xdead_beef;
    r.run(0x889f_fc41, 16); // stlr w1, [x2]
    assert_eq!(r.word(16, 4), 0xdead_beef);
    r.run(0xf8bf_c041, 24); // ldapr x1, [x2]
    assert_eq!(r.x[1], r.word(24, 8));
}
