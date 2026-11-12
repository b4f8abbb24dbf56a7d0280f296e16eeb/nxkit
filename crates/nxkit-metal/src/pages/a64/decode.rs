//! The decoder: plain loads and stores ([`decode`]) and the atomics, exclusives and `LD1`/`ST1` ([`decode_op`]).

use super::*;

fn sext(v: u32, bits: u32) -> i64 {
    let s = 64 - bits;
    ((u64::from(v) << s) as i64) >> s
}

/// Decodes the instruction word `i`.
pub(crate) fn decode(i: u32) -> Result<Mem, Reason> {
    // `DC ZVA, Xt` is `SYS #3, C7, C4, #1, Xt`.
    if i & 0xffff_ffe0 == 0xd50b_7420 {
        return Err(Reason::Zva);
    }
    // Loads and stores: bit 27 set, bit 25 clear.
    if i & 0x0a00_0000 != 0x0800_0000 {
        return Err(Reason::Other);
    }
    // `LD1`..`LD4`, `ST1`..`ST4`, multiple and single structures.
    if i & 0xbe00_0000 == 0x0c00_0000 {
        return Err(Reason::Structure);
    }
    let rt = (i & 31) as u8;
    let rn = ((i >> 5) & 31) as u8;
    let size = ((i >> 30) & 3) as u8;
    let v = (i >> 26) & 1 != 0;
    let opc = ((i >> 22) & 3) as u8;
    // Literal loads.
    if i & 0x3b00_0000 == 0x1800_0000 {
        return Err(Reason::Other);
    }
    {
        // Exclusive and ordered.
        if i & 0x3f00_0000 == 0x0800_0000 {
            let o2 = i >> 23 & 1;
            let l = i >> 22 & 1;
            let o1 = i >> 21 & 1;
            let o0 = i >> 15 & 1;
            if o2 == 0 {
                return Err(Reason::Exclusive);
            }
            if o1 == 1 {
                return Err(Reason::Atomic);
            }
            // `LDAR`, `STLR`: Rs and Rt2 are all ones.
            if o0 == 1 && (i >> 16) & 31 == 31 && (i >> 10) & 31 == 31 {
                let load = l == 1;
                return Ok(Mem {
                    form: if load { Form::Ldar } else { Form::Stlr },
                    load,
                    simd: false,
                    pair: false,
                    size: 1 << size,
                    signed: false,
                    dest64: size == 3,
                    rt,
                    rt2: 0,
                    rn,
                    mode: Mode::Offset(0),
                    order: if load { Order::Acquire } else { Order::Release },
                });
            }
            return Err(Reason::Other);
        }
    }
    match (i >> 27) & 7 {
        // Pairs: `opc 101 V type L imm7 Rt2 Rn Rt`.
        0b101 => {
            let ty = (i >> 23) & 3;
            let load = (i >> 22) & 1 == 1;
            let rt2 = ((i >> 10) & 31) as u8;
            let opc = i >> 30;
            let (bytes, signed, simd) = match (v, opc) {
                (false, 0) => (4u8, false, false),
                (false, 1) if load => (4, true, false),
                (false, 2) => (8, false, false),
                (true, 0) => (4, false, true),
                (true, 1) => (8, false, true),
                (true, 2) => (16, false, true),
                _ => return Err(Reason::Other),
            };
            let scale = bytes.trailing_zeros();
            let imm = sext((i >> 15) & 0x7f, 7) << scale;
            let mode = match ty {
                0 | 2 => Mode::Offset(imm),
                1 => Mode::Post(imm),
                _ => Mode::Pre(imm),
            };
            let form = match (load, simd, signed) {
                (true, false, false) => Form::Ldp,
                (true, false, true) => Form::Ldpsw,
                (true, true, _) => Form::VLdp,
                (false, false, _) => Form::Stp,
                (false, true, _) => Form::VStp,
            };
            Ok(Mem {
                form,
                load,
                simd,
                pair: true,
                size: bytes,
                signed,
                dest64: signed || bytes == 8,
                rt,
                rt2,
                rn,
                mode,
                order: Order::Plain,
            })
        }
        // Single registers.
        0b111 => {
            let kind = (i >> 24) & 3;
            let b21 = (i >> 21) & 1;
            let sub = (i >> 10) & 3;
            if kind == 0 && b21 == 1 && sub == 0 {
                // The group of the atomic memory operations. `LDAPR` is the one load here.
                let is_ldapr = !v && opc == 2 && (i >> 16) & 31 == 31 && (i >> 12) & 15 == 0b1100;
                if is_ldapr {
                    return Ok(Mem {
                        form: Form::Ldar,
                        load: true,
                        simd: false,
                        pair: false,
                        size: 1 << size,
                        signed: false,
                        dest64: size == 3,
                        rt,
                        rt2: 0,
                        rn,
                        mode: Mode::Offset(0),
                        order: Order::Acquire,
                    });
                }
                return Err(if v { Reason::Other } else { Reason::Atomic });
            }
            // The data: bytes, simd, load, signed, dest64.
            let (bytes, simd, load, signed, dest64) = if !v {
                match opc {
                    0 => (1u8 << size, false, false, false, size == 3),
                    1 => (1 << size, false, true, false, size == 3),
                    2 if size == 3 => return Err(Reason::Prefetch),
                    2 => (1 << size, false, true, true, true),
                    _ if size >= 2 => return Err(Reason::Other),
                    _ => (1 << size, false, true, true, false),
                }
            } else {
                match (size, opc) {
                    (s, 0) => (1 << s, true, false, false, false),
                    (s, 1) => (1 << s, true, true, false, false),
                    (0, 2) => (16, true, false, false, false),
                    (0, 3) => (16, true, true, false, false),
                    _ => return Err(Reason::Other),
                }
            };
            let scale = bytes.trailing_zeros();
            let mode = match kind {
                1 => Mode::Offset(i64::from((i >> 10) & 0xfff) << scale),
                0 if b21 == 0 => {
                    let imm = sext((i >> 12) & 0x1ff, 9);
                    match sub {
                        0 => Mode::Offset(imm),
                        1 => Mode::Post(imm),
                        3 => Mode::Pre(imm),
                        // Unprivileged (`LDTR`, `STTR`).
                        _ => return Err(Reason::Other),
                    }
                }
                0 if sub == 2 => {
                    let option = ((i >> 13) & 7) as u8;
                    if !matches!(option, 2 | 3 | 6 | 7) {
                        return Err(Reason::Other);
                    }
                    Mode::Reg {
                        rm: ((i >> 16) & 31) as u8,
                        option,
                        shift: if (i >> 12) & 1 == 1 { scale as u8 } else { 0 },
                    }
                }
                _ => return Err(Reason::Other),
            };
            let form = match (load, simd, signed) {
                (true, false, false) => Form::Ldr,
                (true, false, true) => Form::Ldrs,
                (true, true, _) => Form::VLdr,
                (false, false, _) => Form::Str,
                (false, true, _) => Form::VStr,
            };
            Ok(Mem {
                form,
                load,
                simd,
                pair: false,
                size: bytes,
                signed,
                dest64,
                rt,
                rt2: 0,
                rn,
                mode,
                order: Order::Plain,
            })
        }
        _ => Err(Reason::Other),
    }
}

/// Decodes the instruction word `i` into any operation the emulator knows.
pub(crate) fn decode_op(i: u32) -> Result<Op, Reason> {
    match decode(i) {
        Ok(m) => Ok(Op::Mem(m)),
        Err(r @ (Reason::Atomic | Reason::Exclusive | Reason::Structure)) => {
            decode_special(i).ok_or(r)
        }
        Err(r) => Err(r),
    }
}

fn decode_special(i: u32) -> Option<Op> {
    let rt = (i & 31) as u8;
    let rn = ((i >> 5) & 31) as u8;
    let rs = ((i >> 16) & 31) as u8;
    let size = ((i >> 30) & 3) as u8;
    // Atomic memory operations: `size 111 0 00 A R 1 Rs o3 opc 00 Rn Rt`.
    if i & 0x3f20_0c00 == 0x3820_0000 {
        let (acq, rel) = (i >> 23 & 1 == 1, i >> 22 & 1 == 1);
        let o3 = i >> 15 & 1;
        let op = match (o3, (i >> 12) & 7) {
            (0, 0) => RmwOp::Add,
            (0, 1) => RmwOp::Clr,
            (0, 2) => RmwOp::Eor,
            (0, 3) => RmwOp::Set,
            (0, 4) => RmwOp::Smax,
            (0, 5) => RmwOp::Smin,
            (0, 6) => RmwOp::Umax,
            (0, 7) => RmwOp::Umin,
            (1, 0) => RmwOp::Swp,
            _ => return None,
        };
        return Some(Op::Rmw {
            op,
            size: 1 << size,
            rs,
            rt,
            rn,
            acq,
            rel,
        });
    }
    // `CAS`: `size 001000 1 L 1 Rs o0 11111 Rn Rt`.
    if i & 0x3fa0_7c00 == 0x08a0_7c00 {
        return Some(Op::Cas {
            size: 1 << size,
            pair: false,
            rs,
            rt,
            rn,
            acq: i >> 22 & 1 == 1,
            rel: i >> 15 & 1 == 1,
        });
    }
    // `CASP`: `0 sz 001000 0 L 1 Rs o0 11111 Rn Rt`.
    if i & 0xbfa0_7c00 == 0x0820_7c00 {
        // Even registers only.
        if !rs.is_multiple_of(2) || !rt.is_multiple_of(2) {
            return None;
        }
        return Some(Op::Cas {
            size: 4 << size,
            pair: true,
            rs,
            rt,
            rn,
            acq: i >> 22 & 1 == 1,
            rel: i >> 15 & 1 == 1,
        });
    }
    // `LDXR`, `STXR`: `size 001000 0 L 0 Rs o0 Rt2 Rn Rt`.
    if i & 0x3fa0_0000 == 0x0800_0000 {
        let load = i >> 22 & 1 == 1;
        if load && (rs != 31 || (i >> 10) & 31 != 31) {
            return None;
        }
        return Some(Op::Excl {
            load,
            pair: false,
            size: 1 << size,
            rs,
            rt,
            rt2: 31,
            rn,
            ord: i >> 15 & 1 == 1,
        });
    }
    // `LDXP`, `STXP`: sizes `10` and `11`.
    if i & 0x3fa0_0000 == 0x0820_0000 && size >= 2 {
        let load = i >> 22 & 1 == 1;
        return Some(Op::Excl {
            load,
            pair: true,
            size: 1 << size,
            rs,
            rt,
            rt2: ((i >> 10) & 31) as u8,
            rn,
            ord: i >> 15 & 1 == 1,
        });
    }
    // `LD1`, `ST1` (multiple structures, one to four registers): `0 Q 0011000 L 000000 opcode size Rn Rt`, or with a post
    // index `0 Q 0011001 L 0 Rm opcode size Rn Rt`.
    let plain = i & 0xbfbf_0000 == 0x0c00_0000;
    let postx = i & 0xbfa0_0000 == 0x0c80_0000;
    if plain || postx {
        let regs = match (i >> 12) & 15 {
            0b0111 => 1,
            0b1010 => 2,
            0b0110 => 3,
            0b0010 => 4,
            _ => return None,
        };
        return Some(Op::Ld1 {
            load: i >> 22 & 1 == 1,
            q: i >> 30 & 1 == 1,
            regs,
            rt,
            rn,
            post: postx.then_some(rs),
        });
    }
    // `LD1`, `ST1` (one lane): `0 Q 0011010 L 0 00000 opcode S size Rn Rt`, or with a post index
    // `0 Q 0011011 L 0 Rm opcode S size Rn Rt`.
    let lane_plain = i & 0xbfbf_0000 == 0x0d00_0000;
    let lane_post = i & 0xbfa0_0000 == 0x0d80_0000;
    if lane_plain || lane_post {
        let (q, s_bit, sz) = (i >> 30 & 1, i >> 12 & 1, i >> 10 & 3);
        let (size, index) = match (i >> 13) & 7 {
            0b000 => (1, q << 3 | s_bit << 2 | sz),
            0b010 if sz & 1 == 0 => (2, q << 2 | s_bit << 1 | sz >> 1),
            0b100 if sz == 0 => (4, q << 1 | s_bit),
            0b100 if sz == 1 && s_bit == 0 => (8, q),
            _ => return None,
        };
        return Some(Op::Lane {
            load: i >> 22 & 1 == 1,
            size,
            index: index as u8,
            rt,
            rn,
            post: lane_post.then_some(rs),
        });
    }
    None
}
