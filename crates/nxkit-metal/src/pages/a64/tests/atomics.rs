//! Tests of the decoder and the executor of the atomics, the exclusives and `LD1`/`ST1`.

use super::*;

#[test]
fn atomic_memory_operations_decode_by_operation_size_and_order() {
    let rmw_of = |i: u32| match op(i) {
        Op::Rmw {
            op,
            size,
            rs,
            rt,
            rn,
            acq,
            rel,
        } => (op, size, rs, rt, rn, acq, rel),
        o => panic!("{i:#010x}: {o:?}"),
    };
    assert_eq!(rmw_of(0xf821_0062), (RmwOp::Add, 8, 1, 2, 3, false, false));
    assert_eq!(rmw_of(0xb8a1_0062), (RmwOp::Add, 4, 1, 2, 3, true, false));
    assert_eq!(rmw_of(0xf8e1_0062), (RmwOp::Add, 8, 1, 2, 3, true, true));
    assert_eq!(rmw_of(0x3861_0062), (RmwOp::Add, 1, 1, 2, 3, false, true));
    assert_eq!(rmw_of(0x7821_0062), (RmwOp::Add, 2, 1, 2, 3, false, false));
    assert_eq!(rmw_of(0xf821_1062).0, RmwOp::Clr);
    assert_eq!(rmw_of(0xf821_2062).0, RmwOp::Eor);
    assert_eq!(rmw_of(0xf821_3062).0, RmwOp::Set);
    assert_eq!(rmw_of(0xf821_4062).0, RmwOp::Smax);
    assert_eq!(rmw_of(0xb821_5062).0, RmwOp::Smin);
    assert_eq!(rmw_of(0xf821_6062).0, RmwOp::Umax);
    assert_eq!(rmw_of(0xf821_7062).0, RmwOp::Umin);
    assert_eq!(rmw_of(0xf821_8062).0, RmwOp::Swp);
    assert_eq!(rmw_of(0xb8e1_8062), (RmwOp::Swp, 4, 1, 2, 3, true, true));
    assert_eq!(rmw_of(0xf821_007f).3, 31, "STADD discards the result");
}

#[test]
fn compare_and_swap_exclusives_and_ld1_decode() {
    assert_eq!(
        op(0xc8a1_7c62),
        Op::Cas {
            size: 8,
            pair: false,
            rs: 1,
            rt: 2,
            rn: 3,
            acq: false,
            rel: false
        }
    );
    assert_eq!(
        op(0x88e1_fc62),
        Op::Cas {
            size: 4,
            pair: false,
            rs: 1,
            rt: 2,
            rn: 3,
            acq: true,
            rel: true
        }
    );
    assert_eq!(
        op(0x08a1_7c62),
        Op::Cas {
            size: 1,
            pair: false,
            rs: 1,
            rt: 2,
            rn: 3,
            acq: false,
            rel: false
        }
    );
    assert_eq!(
        op(0x4822_7cc4),
        Op::Cas {
            size: 8,
            pair: true,
            rs: 2,
            rt: 4,
            rn: 6,
            acq: false,
            rel: false
        }
    );
    assert_eq!(
        op(0x0862_fcc4),
        Op::Cas {
            size: 4,
            pair: true,
            rs: 2,
            rt: 4,
            rn: 6,
            acq: true,
            rel: true
        }
    );
    let ex = |i| match op(i) {
        Op::Excl {
            load,
            pair,
            size,
            rs,
            rt,
            rt2,
            rn,
            ord,
        } => (load, pair, size, rs, rt, rt2, rn, ord),
        o => panic!("{i:#010x}: {o:?}"),
    };
    assert_eq!(ex(0xc85f_7c41), (true, false, 8, 31, 1, 31, 2, false));
    assert_eq!(ex(0x885f_fc41), (true, false, 4, 31, 1, 31, 2, true));
    assert_eq!(ex(0x085f_7c41), (true, false, 1, 31, 1, 31, 2, false));
    assert_eq!(ex(0xc803_7c41), (false, false, 8, 3, 1, 31, 2, false));
    assert_eq!(ex(0x8803_fc41), (false, false, 4, 3, 1, 31, 2, true));
    assert_eq!(ex(0x4803_7c41), (false, false, 2, 3, 1, 31, 2, false));
    assert_eq!(ex(0xc87f_0861), (true, true, 8, 31, 1, 2, 3, false));
    assert_eq!(ex(0x887f_8861), (true, true, 4, 31, 1, 2, 3, true));
    assert_eq!(ex(0xc824_0861), (false, true, 8, 4, 1, 2, 3, false));
    assert_eq!(ex(0x8824_8861), (false, true, 4, 4, 1, 2, 3, true));
    let ld1 = |i| match op(i) {
        Op::Ld1 {
            load,
            q,
            regs,
            rt,
            rn,
            post,
        } => (load, q, regs, rt, rn, post),
        o => panic!("{i:#010x}: {o:?}"),
    };
    assert_eq!(ld1(0x4c40_7020), (true, true, 1, 0, 1, None));
    assert_eq!(ld1(0x0c40_7020), (true, false, 1, 0, 1, None));
    assert_eq!(ld1(0x4c40_a820), (true, true, 2, 0, 1, None));
    assert_eq!(ld1(0x4c40_6c20), (true, true, 3, 0, 1, None));
    assert_eq!(ld1(0x4c40_203e), (true, true, 4, 30, 1, None));
    assert_eq!(ld1(0x4c00_7020), (false, true, 1, 0, 1, None));
    assert_eq!(ld1(0x0c9f_a020), (false, false, 2, 0, 1, Some(31)));
    assert_eq!(ld1(0x4c82_7020), (false, true, 1, 0, 1, Some(2)));
    assert_eq!(ld1(0x4cdf_a020), (true, true, 2, 0, 1, Some(31)));
    // What stays out: LD2, LD1R, the lane forms of two registers.
    assert_eq!(decode_op(0x4c40_8820).err(), Some(Reason::Structure));
    assert_eq!(decode_op(0x0d40_c820).err(), Some(Reason::Structure));
    assert_eq!(decode_op(0x0d60_9020).err(), Some(Reason::Structure));
    // One lane: `ST1 {v0.s}[1], [x22]`, `ST1 {v0.s}[2], [x8]`, `LD1 {v0.s}[1], [x1]`, `LD1 {v3.b}[15], [x2], #1`,
    // `ST1 {v1.d}[1], [x2], x3`, `ST1 {v1.h}[5], [x2]`.
    let lane = |i| match op(i) {
        Op::Lane {
            load,
            size,
            index,
            rt,
            rn,
            post,
        } => (load, size, index, rt, rn, post),
        o => panic!("{i:#010x}: {o:?}"),
    };
    assert_eq!(lane(0x0d00_92c0), (false, 4, 1, 0, 22, None));
    assert_eq!(lane(0x4d00_8100), (false, 4, 2, 0, 8, None));
    assert_eq!(lane(0x0d40_9020), (true, 4, 1, 0, 1, None));
    assert_eq!(lane(0x4ddf_1c43), (true, 1, 15, 3, 2, Some(31)));
    assert_eq!(lane(0x4d83_8441), (false, 8, 1, 1, 2, Some(3)));
    assert_eq!(lane(0x4d00_4841), (false, 2, 5, 1, 2, None));
}

#[test]
fn atomic_operations_give_the_old_value_and_store_the_result() {
    let mut r = Rig::new();
    // Aligned positions: the rig buffer has no alignment promise, so find one.
    let a = (8 - (r.mem.as_ptr() as usize) % 8) % 8 + 16;
    let put = |r: &mut Rig, v: u64| r.mem[a..a + 8].copy_from_slice(&v.to_le_bytes());
    let get = |r: &Rig| u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap());
    for (i, f, start, operand) in [
        (
            0xf821_0062u32,
            (|m: u64, v: u64| m.wrapping_add(v)) as fn(u64, u64) -> u64,
            5u64,
            7u64,
        ),
        (0xf821_1062, |m, v| m & !v, 0xff, 0x0f),
        (0xf821_2062, |m, v| m ^ v, 0xf0, 0xff),
        (0xf821_3062, |m, v| m | v, 0xf0, 0x0f),
        (
            0xf821_4062,
            |m, v| (m as i64).max(v as i64) as u64,
            (-5i64) as u64,
            3,
        ),
        (
            0xf821_5062,
            |m, v| (m as i64).min(v as i64) as u64,
            (-5i64) as u64,
            3,
        ),
        (0xf821_6062, |m, v| m.max(v), (-5i64) as u64, 3),
        (0xf821_7062, |m, v| m.min(v), (-5i64) as u64, 3),
        (0xf821_8062, |_, v| v, 9, 3),
    ] {
        put(&mut r, start);
        r.x[1] = operand;
        assert!(r.run_op(i, a));
        assert_eq!(r.x[2], start, "{i:#010x} old value");
        assert_eq!(get(&r), f(start, operand), "{i:#010x} result");
    }
    // A word add wraps at 32 bits and leaves the next bytes alone.
    r.mem[a..a + 8].copy_from_slice(&[0xff, 0xff, 0xff, 0xff, 7, 7, 7, 7]);
    r.x[1] = 2;
    assert!(r.run_op(0xb821_0062, a)); // ldadd w1, w2, [x3]
    assert_eq!(&r.mem[a..a + 8], &[1, 0, 0, 0, 7, 7, 7, 7]);
    assert_eq!(r.x[2], 0xffff_ffff);
    // A misaligned atomic is refused and changes nothing.
    let before = r.mem;
    assert!(!r.run_op(0xf821_0062, a + 1));
    assert_eq!(before, r.mem);
}

#[test]
fn one_lane_stores_and_loads_touch_only_that_lane_and_write_back() {
    let mut r = Rig::new();
    r.v[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
    // `ST1 {v0.s}[1], [x22]` stores the second word.
    let next = r.mem[12];
    assert!(r.run_op(0x0d00_92c0, 8));
    assert_eq!(&r.mem[8..12], &[0x33, 0x33, 0x33, 0x33]);
    assert_eq!(r.mem[12], next, "only four bytes");
    // `LD1 {v0.s}[2], [x1]` (0x4d40_8020) replaces the third word and keeps the others.
    r.mem[20..24].copy_from_slice(&[9, 8, 7, 6]);
    assert!(r.run_op(0x4d40_8020, 20));
    assert_eq!(
        r.v[0], 0x1111_1111_0607_0809_3333_3333_4444_4444u128,
        "lane 2 is the word at bits 64 to 96"
    );
}

#[test]
fn compare_and_swap_and_pairs() {
    let mut r = Rig::new();
    let a = (16 - (r.mem.as_ptr() as usize) % 16) % 16 + 16;
    r.mem[a..a + 8].copy_from_slice(&5u64.to_le_bytes());
    r.x[1] = 5; // expected
    r.x[2] = 9; // new
    assert!(r.run_op(0xc8a1_7c62, a)); // cas x1, x2, [x3]
    assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 9);
    assert_eq!(r.x[1], 5);
    r.x[1] = 5;
    r.x[2] = 11;
    assert!(r.run_op(0xc8a1_7c62, a)); // fails now: memory holds 9
    assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 9);
    assert_eq!(r.x[1], 9, "the old value comes back");
    // CASP of two x registers: compare x2, x3 with the 16 bytes, store x4, x5.
    r.mem[a..a + 8].copy_from_slice(&1u64.to_le_bytes());
    r.mem[a + 8..a + 16].copy_from_slice(&2u64.to_le_bytes());
    (r.x[2], r.x[3], r.x[4], r.x[5]) = (1, 2, 30, 40);
    assert!(r.run_op(0x4822_7cc4, a)); // casp x2, x3, x4, x5, [x6]
    assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 30);
    assert_eq!(
        u64::from_le_bytes(r.mem[a + 8..a + 16].try_into().unwrap()),
        40
    );
    // CASP of two w registers.
    r.mem[a..a + 4].copy_from_slice(&7u32.to_le_bytes());
    r.mem[a + 4..a + 8].copy_from_slice(&8u32.to_le_bytes());
    (r.x[2], r.x[3], r.x[4], r.x[5]) = (7, 8, 70, 80);
    assert!(r.run_op(0x0862_fcc4, a)); // caspal w2, w3, w4, w5, [x6]
    assert_eq!(&r.mem[a..a + 8], &[70, 0, 0, 0, 80, 0, 0, 0]);
}

#[test]
fn a_store_exclusive_succeeds_only_after_its_load_exclusive_of_the_same_address() {
    let mut r = Rig::new();
    let a = (16 - (r.mem.as_ptr() as usize) % 16) % 16 + 16;
    r.mem[a..a + 8].copy_from_slice(&5u64.to_le_bytes());
    // stxr without a load: fails.
    r.x[1] = 99;
    assert!(r.run_op(0xc803_7c41, a)); // stxr w3, x1, [x2]
    assert_eq!(r.x[3], 1);
    assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 5);
    // ldxr then stxr: succeeds.
    assert!(r.run_op(0xc85f_7c41, a)); // ldxr x1, [x2]
    assert_eq!(r.x[1], 5);
    r.x[1] = 6;
    assert!(r.run_op(0xc803_7c41, a));
    assert_eq!(r.x[3], 0);
    assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 6);
    // The monitor is gone after the store: a second store fails.
    assert!(r.run_op(0xc803_7c41, a));
    assert_eq!(r.x[3], 1);
    // Memory changed in between: the store fails.
    assert!(r.run_op(0xc85f_7c41, a));
    r.mem[a] = 77;
    r.x[1] = 1;
    assert!(r.run_op(0xc803_7c41, a));
    assert_eq!(r.x[3], 1);
    // A load of another address does not satisfy a store to this one.
    assert!(r.run_op(0xc85f_7c41, a + 8));
    assert!(r.run_op(0xc803_7c41, a));
    assert_eq!(r.x[3], 1);
    // Pairs.
    r.mem[a..a + 8].copy_from_slice(&1u64.to_le_bytes());
    r.mem[a + 8..a + 16].copy_from_slice(&2u64.to_le_bytes());
    assert!(r.run_op(0xc87f_0861, a)); // ldxp x1, x2, [x3]
    assert_eq!((r.x[1], r.x[2]), (1, 2));
    (r.x[1], r.x[2]) = (10, 20);
    assert!(r.run_op(0xc824_0861, a)); // stxp w4, x1, x2, [x3]
    assert_eq!(r.x[4], 0);
    assert_eq!(u64::from_le_bytes(r.mem[a..a + 8].try_into().unwrap()), 10);
    assert_eq!(
        u64::from_le_bytes(r.mem[a + 8..a + 16].try_into().unwrap()),
        20
    );
}

#[test]
fn ld1_and_st1_move_whole_registers_in_a_row() {
    let mut r = Rig::new();
    assert!(r.run_op(0x4c40_a820, 0)); // ld1 {v0.4s, v1.4s}, [x1]
    assert_eq!(
        r.v[0],
        u128::from_le_bytes(r.mem[0..16].try_into().unwrap())
    );
    assert_eq!(
        r.v[1],
        u128::from_le_bytes(r.mem[16..32].try_into().unwrap())
    );
    assert!(r.run_op(0x0c40_7020, 40)); // ld1 {v0.8b}, [x1]: the upper half clears
    assert_eq!(r.v[0], u128::from(r.word(40, 8)));
    // ld1 of four registers wrapping from v30 to v1.
    assert!(r.run_op(0x4c40_203e, 0));
    assert_eq!(
        r.v[30],
        u128::from_le_bytes(r.mem[0..16].try_into().unwrap())
    );
    assert_eq!(
        r.v[1],
        u128::from_le_bytes(r.mem[48..64].try_into().unwrap())
    );
    // st1 with an immediate post index writes the base back.
    r.v[0] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    r.v[1] = 0x1f1e_1d1c_1b1a_1918_1716_1514_1312_1110;
    assert!(r.run_op(0x0c9f_a020, 64)); // st1 {v0.8b, v1.8b}, [x1], #16
    assert_eq!(r.word(64, 8), 0x0706_0504_0302_0100);
    assert_eq!(r.word(72, 8), 0x1716_1514_1312_1110);
    assert_eq!(r.x[1], r.mem.as_ptr() as u64 + 64 + 16);
    // A register post index.
    r.x[2] = 24;
    assert!(r.run_op(0x4c82_7020, 80)); // st1 {v0.16b}, [x1], x2
    assert_eq!(r.x[1], r.mem.as_ptr() as u64 + 80 + 24);
}
