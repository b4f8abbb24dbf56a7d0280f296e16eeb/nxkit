//! Tests of the decoder of plain loads and stores.

use super::*;

#[test]
fn unsigned_offset_loads_and_stores() {
    check(0xf940_0841, Ldr, 8, 1, 0, 2, Offset(16), false, false);
    check(0xb940_0c41, Ldr, 4, 1, 0, 2, Offset(12), false, false);
    check(0xf940_07e1, Ldr, 8, 1, 0, 31, Offset(8), false, false);
    check(0x3940_1483, Ldr, 1, 3, 0, 4, Offset(5), false, false);
    check(0x7940_0c83, Ldr, 2, 3, 0, 4, Offset(6), false, false);
    check(0x3980_1483, Ldrs, 1, 3, 0, 4, Offset(5), true, true);
    check(0x39c0_1483, Ldrs, 1, 3, 0, 4, Offset(5), true, false);
    check(0x7980_0c83, Ldrs, 2, 3, 0, 4, Offset(6), true, true);
    check(0x79c0_0c83, Ldrs, 2, 3, 0, 4, Offset(6), true, false);
    check(0xb980_0883, Ldrs, 4, 3, 0, 4, Offset(8), true, true);
    check(0xf900_0841, Str, 8, 1, 0, 2, Offset(16), false, false);
    check(0xb900_0c41, Str, 4, 1, 0, 2, Offset(12), false, false);
    check(0xf900_0c5f, Str, 8, 31, 0, 2, Offset(24), false, false);
    check(0x3900_1483, Str, 1, 3, 0, 4, Offset(5), false, false);
    check(0x7900_0c83, Str, 2, 3, 0, 4, Offset(6), false, false);
}

#[test]
fn unscaled_offsets() {
    check(0xf85f_8041, Ldr, 8, 1, 0, 2, Offset(-8), false, false);
    check(0xb840_3041, Ldr, 4, 1, 0, 2, Offset(3), false, false);
    check(0x385f_f041, Ldr, 1, 1, 0, 2, Offset(-1), false, false);
    check(0x785f_e041, Ldr, 2, 1, 0, 2, Offset(-2), false, false);
    check(0x389f_f041, Ldrs, 1, 1, 0, 2, Offset(-1), true, true);
    check(0x78df_e041, Ldrs, 2, 1, 0, 2, Offset(-2), true, false);
    check(0xb89f_c041, Ldrs, 4, 1, 0, 2, Offset(-4), true, true);
    check(0xf81f_8041, Str, 8, 1, 0, 2, Offset(-8), false, false);
    check(0x381f_f041, Str, 1, 1, 0, 2, Offset(-1), false, false);
    check(0x781f_e041, Str, 2, 1, 0, 2, Offset(-2), false, false);
    check(0xb81f_c041, Str, 4, 1, 0, 2, Offset(-4), false, false);
}

#[test]
fn pre_and_post_index() {
    check(0xf841_0c41, Ldr, 8, 1, 0, 2, Pre(16), false, false);
    check(0xb85f_4c41, Ldr, 4, 1, 0, 2, Pre(-12), false, false);
    check(0xf841_0441, Ldr, 8, 1, 0, 2, Post(16), false, false);
    check(0xb85f_c441, Ldr, 4, 1, 0, 2, Post(-4), false, false);
    check(0xf81f_0c41, Str, 8, 1, 0, 2, Pre(-16), false, false);
    check(0xf800_8441, Str, 8, 1, 0, 2, Post(8), false, false);
    check(0x3800_1441, Str, 1, 1, 0, 2, Post(1), false, false);
    check(0x3840_1c41, Ldr, 1, 1, 0, 2, Pre(1), false, false);
}

#[test]
fn register_offsets() {
    check(
        0xf863_6841,
        Ldr,
        8,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 0,
        },
        false,
        false,
    );
    check(
        0xf863_7841,
        Ldr,
        8,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 3,
        },
        false,
        false,
    );
    check(
        0xb863_5841,
        Ldr,
        4,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 2,
            shift: 2,
        },
        false,
        false,
    );
    check(
        0xb863_c841,
        Ldr,
        4,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 6,
            shift: 0,
        },
        false,
        false,
    );
    check(
        0xf863_d841,
        Ldr,
        8,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 6,
            shift: 3,
        },
        false,
        false,
    );
    check(
        0xf863_f841,
        Ldr,
        8,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 7,
            shift: 3,
        },
        false,
        false,
    );
    check(
        0x3863_6841,
        Ldr,
        1,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 0,
        },
        false,
        false,
    );
    check(
        0x3863_4841,
        Ldr,
        1,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 2,
            shift: 0,
        },
        false,
        false,
    );
    check(
        0x7863_7841,
        Ldr,
        2,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 1,
        },
        false,
        false,
    );
    check(
        0xb8a3_7841,
        Ldrs,
        4,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 2,
        },
        true,
        true,
    );
    check(
        0x38e3_6841,
        Ldrs,
        1,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 0,
        },
        true,
        false,
    );
    check(
        0x78a3_7841,
        Ldrs,
        2,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 1,
        },
        true,
        true,
    );
    check(
        0xf823_7841,
        Str,
        8,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 3,
        },
        false,
        false,
    );
    check(
        0xb823_5841,
        Str,
        4,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 2,
            shift: 2,
        },
        false,
        false,
    );
    check(
        0x3823_6841,
        Str,
        1,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 0,
        },
        false,
        false,
    );
    check(
        0x7823_7841,
        Str,
        2,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 1,
        },
        false,
        false,
    );
}

#[test]
fn pairs() {
    check(0xa941_0861, Ldp, 8, 1, 2, 3, Offset(16), false, false);
    check(0x2941_0861, Ldp, 4, 1, 2, 3, Offset(8), false, false);
    check(0xa9ff_0861, Ldp, 8, 1, 2, 3, Pre(-16), false, false);
    check(0xa8c1_0861, Ldp, 8, 1, 2, 3, Post(16), false, false);
    check(0xa901_0861, Stp, 8, 1, 2, 3, Offset(16), false, false);
    check(0x29bf_0be1, Stp, 4, 1, 2, 31, Pre(-8), false, false);
    check(0xa8be_0861, Stp, 8, 1, 2, 3, Post(-32), false, false);
    check(0xa900_fc7f, Stp, 8, 31, 31, 3, Offset(8), false, false);
    check(0x6941_0861, Ldpsw, 4, 1, 2, 3, Offset(8), true, true);
    check(0x68c1_0861, Ldpsw, 4, 1, 2, 3, Post(8), true, true);
    check(0x69ff_0861, Ldpsw, 4, 1, 2, 3, Pre(-8), true, true);
    check(0xa841_0861, Ldp, 8, 1, 2, 3, Offset(16), false, false);
    check(0xa801_0861, Stp, 8, 1, 2, 3, Offset(16), false, false);
}

#[test]
fn simd_single_registers() {
    check(0x3d40_0c41, VLdr, 1, 1, 0, 2, Offset(3), false, false);
    check(0x7d40_0c41, VLdr, 2, 1, 0, 2, Offset(6), false, false);
    check(0xbd40_0c41, VLdr, 4, 1, 0, 2, Offset(12), false, false);
    check(0xfd40_0841, VLdr, 8, 1, 0, 2, Offset(16), false, false);
    check(0x3dc0_0841, VLdr, 16, 1, 0, 2, Offset(32), false, false);
    check(0x3d00_0c41, VStr, 1, 1, 0, 2, Offset(3), false, false);
    check(0x7d00_0c41, VStr, 2, 1, 0, 2, Offset(6), false, false);
    check(0xbd00_0c41, VStr, 4, 1, 0, 2, Offset(12), false, false);
    check(0xfd00_0841, VStr, 8, 1, 0, 2, Offset(16), false, false);
    check(0x3d80_0841, VStr, 16, 1, 0, 2, Offset(32), false, false);
    check(0x3c5f_f041, VLdr, 1, 1, 0, 2, Offset(-1), false, false);
    check(0x3cde_f041, VLdr, 16, 1, 0, 2, Offset(-17), false, false);
    check(0xfc1f_7041, VStr, 8, 1, 0, 2, Offset(-9), false, false);
    check(0x3c9f_0041, VStr, 16, 1, 0, 2, Offset(-16), false, false);
    check(0x3cc1_0c41, VLdr, 16, 1, 0, 2, Pre(16), false, false);
    check(0x3cc1_0441, VLdr, 16, 1, 0, 2, Post(16), false, false);
    check(0x3c9f_0c41, VStr, 16, 1, 0, 2, Pre(-16), false, false);
    check(0xfc00_8441, VStr, 8, 1, 0, 2, Post(8), false, false);
    check(
        0x3ce3_7841,
        VLdr,
        16,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 4,
        },
        false,
        false,
    );
    check(
        0xfc63_7841,
        VLdr,
        8,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 3,
        },
        false,
        false,
    );
    check(
        0xbc63_5841,
        VLdr,
        4,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 2,
            shift: 2,
        },
        false,
        false,
    );
    check(
        0x3c63_6841,
        VLdr,
        1,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 0,
        },
        false,
        false,
    );
    check(
        0x3ca3_7841,
        VStr,
        16,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 4,
        },
        false,
        false,
    );
    check(
        0x7c23_7841,
        VStr,
        2,
        1,
        0,
        2,
        Reg {
            rm: 3,
            option: 3,
            shift: 1,
        },
        false,
        false,
    );
}

#[test]
fn simd_pairs() {
    check(0x2d41_0861, VLdp, 4, 1, 2, 3, Offset(8), false, false);
    check(0x6d41_0861, VLdp, 8, 1, 2, 3, Offset(16), false, false);
    check(0xad41_0861, VLdp, 16, 1, 2, 3, Offset(32), false, false);
    check(0xadbf_0861, VStp, 16, 1, 2, 3, Pre(-32), false, false);
    check(0x6c81_0861, VStp, 8, 1, 2, 3, Post(16), false, false);
    check(0x2d00_8861, VStp, 4, 1, 2, 3, Offset(4), false, false);
    check(0xac41_0861, VLdp, 16, 1, 2, 3, Offset(32), false, false);
    check(0x6c02_0861, VStp, 8, 1, 2, 3, Offset(32), false, false);
}

#[test]
fn ordered_accesses() {
    for (i, form, size, order) in [
        (0xc8df_fc41, Ldar, 8, Order::Acquire),
        (0x88df_fc41, Ldar, 4, Order::Acquire),
        (0x08df_fc41, Ldar, 1, Order::Acquire),
        (0x48df_fc41, Ldar, 2, Order::Acquire),
        (0xc89f_fc41, Stlr, 8, Order::Release),
        (0x889f_fc41, Stlr, 4, Order::Release),
        (0x089f_fc41, Stlr, 1, Order::Release),
        (0x489f_fc41, Stlr, 2, Order::Release),
        (0xf8bf_c041, Ldar, 8, Order::Acquire),
        (0xb8bf_c041, Ldar, 4, Order::Acquire),
        (0x38bf_c041, Ldar, 1, Order::Acquire),
        (0x78bf_c041, Ldar, 2, Order::Acquire),
    ] {
        let m = d(i);
        assert_eq!((m.form, m.size, m.order), (form, size, order), "{i:#010x}");
        assert_eq!((m.rt, m.rn, m.mode), (1, 2, Offset(0)), "{i:#010x}");
    }
}

#[test]
fn what_is_not_emulated_has_a_reason() {
    for (i, why) in [
        (0xc85f_7c41, Reason::Exclusive),
        (0xc803_7c41, Reason::Exclusive),
        (0xc85f_fc41, Reason::Exclusive),
        (0xc803_fc41, Reason::Exclusive),
        (0xc87f_0861, Reason::Exclusive),
        (0xf821_0062, Reason::Atomic),
        (0xf821_8082, Reason::Atomic),
        (0xc8a1_7c62, Reason::Atomic),
        (0x88e1_fc62, Reason::Atomic),
        (0xf8e1_0062, Reason::Atomic),
        (0xf980_0440, Reason::Prefetch),
        (0xf8a3_6840, Reason::Prefetch),
        (0xf89f_8040, Reason::Prefetch),
        (0x5800_0021, Reason::Other),
        (0x9c00_0001, Reason::Other),
        (0xf840_8841, Reason::Other),
        (0xf800_8841, Reason::Other),
        (0x4c40_7020, Reason::Structure),
        (0x4c00_a820, Reason::Structure),
        (0x0d40_9020, Reason::Structure),
        (0x4d40_c820, Reason::Structure),
        (0x4cdf_7020, Reason::Structure),
        (0xd50b_7421, Reason::Zva),
        (0x8b03_0041, Reason::Other),
    ] {
        assert_eq!(decode(i).err(), Some(why), "{i:#010x}");
    }
}
