//! `InstEmitIntegerArithmetic.cs`.

use super::helper::*;
use crate::emit::emitter::EmitterContext;
use crate::emit::helper::*;
use crate::ir::ops::{Opd, OperandType};
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

fn set_iadd_flags(ctx: &mut Ctx, res: Opd, a: Opd, b: Opd, set_cc: bool, extended: bool) {
    if !set_cc {
        return;
    }
    if extended {
        // C = (d == a && CIn) || d < a
        let mut c0 = ctx.i_compare_equal(res, a);
        let c1 = ctx.i_compare_less_unsigned(res, a);
        let cf = get_cf(ctx);
        c0 = ctx.bitwise_and(c0, cf);
        let cf = get_cf(ctx);
        let o = ctx.bitwise_or(c0, c1);
        ctx.copy_to(cf, o);
    } else {
        // C = d < a
        let cf = get_cf(ctx);
        let lt = ctx.i_compare_less_unsigned(res, a);
        ctx.copy_to(cf, lt);
    }
    // V = (d ^ a) & ~(a ^ b) < 0
    let v0 = ctx.bitwise_exclusive_or(res, a);
    let v1 = ctx.bitwise_exclusive_or(a, b);
    let v1 = ctx.bitwise_not(v1);
    let v = ctx.bitwise_and(v0, v1);
    let vf = get_vf(ctx);
    let z = ctx.ir.const_(0);
    let lt = ctx.i_compare_less(v, z);
    ctx.copy_to(vf, lt);
    set_zn_flags(ctx, res, true, extended);
}

fn emit_iadd(ctx: &mut Ctx, a: Opd, b: Opd, rd: i32, avg: AvgMode, extended: bool, wcc: bool) {
    let a = ctx.i_negate_if(a, avg == AvgMode::NegA);
    let b = ctx.i_negate_if(b, avg == AvgMode::NegB);
    let mut res = ctx.i_add(a, b);
    if extended {
        let cf = get_cf(ctx);
        let one = ctx.ir.const_(1);
        let m = ctx.bitwise_and(cf, one);
        res = ctx.i_add(res, m);
    }
    set_iadd_flags(ctx, res, a, b, wcc, extended);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

#[allow(clippy::too_many_arguments)]
fn emit_iadd3(
    ctx: &mut Ctx,
    mode: Lrs,
    a: Opd,
    b: Opd,
    c: Opd,
    parts: [HalfSelect; 3],
    rd: i32,
    negs: [bool; 3],
) {
    fn ext(ctx: &mut Ctx, src: Opd, part: HalfSelect) -> Opd {
        match part {
            HalfSelect::B32 => src,
            HalfSelect::H0 => {
                let m = ctx.ir.const_(0xffff);
                ctx.bitwise_and(src, m)
            }
            HalfSelect::H1 => {
                let s = ctx.ir.const_(16);
                ctx.shift_right_u32(src, s)
            }
            p => {
                ctx.acc
                    .log(&format!("Iadd3 has invalid component selection {}.", p.0));
                src
            }
        }
    }
    let ea = ext(ctx, a, parts[0]);
    let a = ctx.i_negate_if(ea, negs[0]);
    let eb = ext(ctx, b, parts[1]);
    let b = ctx.i_negate_if(eb, negs[1]);
    let ec = ext(ctx, c, parts[2]);
    let c = ctx.i_negate_if(ec, negs[2]);
    let mut res = ctx.i_add(a, b);
    if mode == Lrs::LeftShift {
        let s = ctx.ir.const_(16);
        res = ctx.shift_left(res, s);
    } else if mode == Lrs::RightShift {
        let s = ctx.ir.const_(16);
        res = ctx.shift_right_u32(res, s);
    }
    res = ctx.i_add(res, c);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

#[allow(clippy::too_many_arguments)]
fn emit_imad(
    ctx: &mut Ctx,
    a: Opd,
    b: Opd,
    c: Opd,
    rd: i32,
    avg: AvgMode,
    signed_a: bool,
    signed_b: bool,
    high: bool,
) {
    let b = ctx.i_negate_if(b, avg == AvgMode::NegA);
    let c = ctx.i_negate_if(c, avg == AvgMode::NegB);
    let mut res;
    if high {
        if signed_a && signed_b {
            res = ctx.multiply_high_s32(a, b);
        } else {
            res = ctx.multiply_high_u32(a, b);
            if signed_a {
                let s = ctx.ir.const_(31);
                let sh = ctx.shift_right_s32(a, s);
                let m = ctx.i_multiply(b, sh);
                res = ctx.i_add(res, m);
            } else if signed_b {
                let s = ctx.ir.const_(31);
                let sh = ctx.shift_right_s32(b, s);
                let m = ctx.i_multiply(a, sh);
                res = ctx.i_add(res, m);
            }
        }
    } else {
        res = ctx.i_multiply(a, b);
    }
    if ctx.ir.ty(c) != OperandType::Constant || ctx.ir.value(c) != 0 {
        res = ctx.i_add(res, c);
    }
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn emit_iscadd(ctx: &mut Ctx, a: Opd, b: Opd, rd: i32, shift: i32, avg: AvgMode, wcc: bool) {
    let s = ctx.ir.const_(shift);
    let a = ctx.shift_left(a, s);
    let a = ctx.i_negate_if(a, avg == AvgMode::NegA);
    let b = ctx.i_negate_if(b, avg == AvgMode::NegB);
    let res = ctx.i_add(a, b);
    set_iadd_flags(ctx, res, a, b, wcc, false);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn emit_lea(ctx: &mut Ctx, a: Opd, b: Opd, rd: i32, neg_a: bool, shift: i32) {
    let s = ctx.ir.const_(shift);
    let a = ctx.shift_left(a, s);
    let a = ctx.i_negate_if(a, neg_a);
    let res = ctx.i_add(a, b);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn emit_lea_hi(ctx: &mut Ctx, a: Opd, b: Opd, c: Opd, rd: i32, neg_a: bool, shift: i32) {
    let s = ctx.ir.const_(shift);
    let mut a_low = ctx.shift_left(a, s);
    let mut a_high = if shift == 0 {
        ctx.ir.const_(0)
    } else {
        let s = ctx.ir.const_(32 - shift);
        ctx.shift_right_u32(a, s)
    };
    let s = ctx.ir.const_(shift);
    let cs = ctx.shift_left(c, s);
    a_high = ctx.bitwise_or(a_high, cs);
    if neg_a {
        a_low = ctx.bitwise_not(a_low);
        a_high = ctx.bitwise_not(a_high);
        let one = ctx.ir.const_(1);
        let (_, cout) = add_with_carry(ctx, a_low, one);
        a_high = ctx.i_add(a_high, cout);
    }
    let res = ctx.i_add(a_high, b);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

fn extend16_to32(ctx: &mut Ctx, src: Opd, high: bool, signed: bool) -> Opd {
    if signed && high {
        let s = ctx.ir.const_(16);
        ctx.shift_right_s32(src, s)
    } else if signed {
        let z = ctx.ir.const_(0);
        let s = ctx.ir.const_(16);
        ctx.bitfield_extract_s32(src, z, s)
    } else if high {
        let s = ctx.ir.const_(16);
        ctx.shift_right_u32(src, s)
    } else {
        let m = ctx.ir.const_(0xffff);
        ctx.bitwise_and(src, m)
    }
}

/// `EmitXmad` with the `XmadCop2` mode (the decoder's `Cbcc` has no encoding there).
#[allow(clippy::too_many_arguments)]
fn emit_xmad2(ctx: &mut Ctx, mode: XmadCop2, a: Opd, b: Opd, c: Opd, rd: i32, flags: XmadFlags) {
    let conv = match mode {
        XmadCop2::Cfull => XmadCop::Cfull,
        XmadCop2::Clo => XmadCop::Clo,
        XmadCop2::Chi => XmadCop::Chi,
        XmadCop2::Csfu => XmadCop::Csfu,
        m => {
            ctx.acc.log(&format!("Invalid XMAD mode \"{}\".", m.0));
            return;
        }
    };
    emit_xmad(ctx, conv, a, b, c, rd, flags);
}

#[derive(Clone, Copy)]
struct XmadFlags {
    signed_a: bool,
    signed_b: bool,
    high_a: bool,
    high_b: bool,
    psl: bool,
    merge: bool,
    extended: bool,
    write_cc: bool,
}

fn emit_xmad(ctx: &mut Ctx, mode: XmadCop, a: Opd, b: Opd, c: Opd, rd: i32, f: XmadFlags) {
    let b_unmodified = b;
    let a = extend16_to32(ctx, a, f.high_a, f.signed_a);
    let b = extend16_to32(ctx, b, f.high_b, f.signed_b);
    let mut res = ctx.i_multiply(a, b);
    if f.psl {
        let s = ctx.ir.const_(16);
        res = ctx.shift_left(res, s);
    }
    let mut c = c;
    match mode {
        XmadCop::Cfull => {}
        XmadCop::Clo => c = extend16_to32(ctx, c, false, false),
        XmadCop::Chi => c = extend16_to32(ctx, c, true, false),
        XmadCop::Cbcc => {
            let s = ctx.ir.const_(16);
            let sh = ctx.shift_left(b_unmodified, s);
            c = ctx.i_add(c, sh);
        }
        XmadCop::Csfu => {
            let s31 = ctx.ir.const_(31);
            let ha = ctx.shift_right_u32(a, s31);
            let s16 = ctx.ir.const_(16);
            let adj_a = ctx.shift_left(ha, s16);
            let s31 = ctx.ir.const_(31);
            let hb = ctx.shift_right_u32(b, s31);
            let s16 = ctx.ir.const_(16);
            let adj_b = ctx.shift_left(hb, s16);
            let sum = ctx.i_add(adj_a, adj_b);
            c = ctx.i_subtract(c, sum);
        }
        m => {
            ctx.acc.log(&format!("Invalid XMAD mode \"{}\".", m.0));
            return;
        }
    }
    let product = res;
    if f.extended {
        let cf = get_cf(ctx);
        let one = ctx.ir.const_(1);
        let m = ctx.bitwise_and(cf, one);
        res = ctx.i_add(res, m);
    } else {
        res = ctx.i_add(res, c);
    }
    set_iadd_flags(ctx, res, product, c, f.write_cc, f.extended);
    if f.merge {
        let m = ctx.ir.const_(0xffff);
        res = ctx.bitwise_and(res, m);
        let s = ctx.ir.const_(16);
        let sh = ctx.shift_left(b_unmodified, s);
        res = ctx.bitwise_or(res, sh);
    }
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    macro_rules! iadd {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_iadd(ctx, a, b, op.dest(), op.avg_mode(), op.x(), op.write_cc());
        }};
    }
    macro_rules! iadd3 {
        ($ty:ident, R) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_b(), false);
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_iadd3(
                ctx,
                op.lrs(),
                a,
                b,
                c,
                [op.apart(), op.bpart(), op.cpart()],
                op.dest(),
                [op.neg_a(), op.neg_b(), op.neg_c()],
            );
        }};
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let c = get_src_reg(ctx, op.src_c(), false);
            let p = [HalfSelect::B32; 3];
            emit_iadd3(
                ctx,
                Lrs::None,
                a,
                b,
                c,
                p,
                op.dest(),
                [op.neg_a(), op.neg_b(), op.neg_c()],
            );
        }};
    }
    macro_rules! imad {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_imad(
                ctx,
                a,
                b,
                c,
                op.dest(),
                op.avg_mode(),
                op.a_signed(),
                op.b_signed(),
                op.hilo(),
            );
        }};
    }
    macro_rules! imul {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let z = ctx.ir.const_(0);
            emit_imad(
                ctx,
                a,
                b,
                z,
                op.dest(),
                AvgMode::NoNeg,
                op.a_signed(),
                op.b_signed(),
                op.hilo(),
            );
        }};
    }
    macro_rules! iscadd {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_iscadd(
                ctx,
                a,
                b,
                op.dest(),
                op.imm5(),
                op.avg_mode(),
                op.write_cc(),
            );
        }};
    }
    macro_rules! lea {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_lea(ctx, a, b, op.dest(), op.neg_a(), op.imm_u5());
        }};
    }
    macro_rules! leahi {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_lea_hi(ctx, a, b, c, op.dest(), op.neg_a(), op.imm_u5());
        }};
    }
    match emit {
        Emit::IaddR => iadd!(InstIaddR, R),
        Emit::IaddI => iadd!(InstIaddI, S),
        Emit::IaddC => iadd!(InstIaddC, C),
        Emit::Iadd32i => iadd!(InstIadd32i, I32),
        Emit::Iadd3R => iadd3!(InstIadd3R, R),
        Emit::Iadd3I => iadd3!(InstIadd3I, S),
        Emit::Iadd3C => iadd3!(InstIadd3C, C),
        Emit::ImadR => imad!(InstImadR, R),
        Emit::ImadI => imad!(InstImadI, S),
        Emit::ImadC => imad!(InstImadC, C),
        Emit::ImadRc => {
            let op = InstImadRc(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_c(), false);
            let c = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            emit_imad(
                ctx,
                a,
                b,
                c,
                op.dest(),
                op.avg_mode(),
                op.a_signed(),
                op.b_signed(),
                op.hilo(),
            );
        }
        Emit::Imad32i => {
            let op = InstImad32i(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_imm(ctx, op.imm32(), false);
            let c = get_src_reg(ctx, op.dest(), false);
            emit_imad(
                ctx,
                a,
                b,
                c,
                op.dest(),
                op.avg_mode(),
                op.a_signed(),
                op.b_signed(),
                op.hilo(),
            );
        }
        Emit::ImulR => imul!(InstImulR, R),
        Emit::ImulI => imul!(InstImulI, S),
        Emit::ImulC => imul!(InstImulC, C),
        Emit::Imul32i => imul!(InstImul32i, I32),
        Emit::IscaddR => iscadd!(InstIscaddR, R),
        Emit::IscaddI => iscadd!(InstIscaddI, S),
        Emit::IscaddC => iscadd!(InstIscaddC, C),
        Emit::Iscadd32i => {
            let op = InstIscadd32i(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_imm(ctx, op.imm32(), false);
            emit_iscadd(
                ctx,
                a,
                b,
                op.dest(),
                op.imm5(),
                AvgMode::NoNeg,
                op.write_cc(),
            );
        }
        Emit::LeaR => lea!(InstLeaR, R),
        Emit::LeaI => lea!(InstLeaI, S),
        Emit::LeaC => lea!(InstLeaC, C),
        Emit::LeaHiR => leahi!(InstLeaHiR, R),
        Emit::LeaHiC => leahi!(InstLeaHiC, C),
        Emit::XmadR => {
            let op = InstXmadR(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_b(), false);
            let c = get_src_reg(ctx, op.src_c(), false);
            let f = XmadFlags {
                signed_a: op.a_signed(),
                signed_b: op.b_signed(),
                high_a: op.hilo_a(),
                high_b: op.hilo_b(),
                psl: op.psl(),
                merge: op.mrg(),
                extended: op.x(),
                write_cc: op.write_cc(),
            };
            emit_xmad(ctx, op.xmad_cop(), a, b, c, op.dest(), f);
        }
        Emit::XmadI => {
            let op = InstXmadI(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_imm(ctx, op.imm16(), false);
            let c = get_src_reg(ctx, op.src_c(), false);
            let f = XmadFlags {
                signed_a: op.a_signed(),
                signed_b: op.b_signed(),
                high_a: op.hilo_a(),
                high_b: false,
                psl: op.psl(),
                merge: op.mrg(),
                extended: op.x(),
                write_cc: op.write_cc(),
            };
            emit_xmad(ctx, op.xmad_cop(), a, b, c, op.dest(), f);
        }
        Emit::XmadC => {
            let op = InstXmadC(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            let c = get_src_reg(ctx, op.src_c(), false);
            let f = XmadFlags {
                signed_a: op.a_signed(),
                signed_b: op.b_signed(),
                high_a: op.hilo_a(),
                high_b: op.hilo_b(),
                psl: op.psl(),
                merge: op.mrg(),
                extended: op.x(),
                write_cc: op.write_cc(),
            };
            emit_xmad2(ctx, op.xmad_cop(), a, b, c, op.dest(), f);
        }
        Emit::XmadRc => {
            let op = InstXmadRc(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_c(), false);
            let c = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            let f = XmadFlags {
                signed_a: op.a_signed(),
                signed_b: op.b_signed(),
                high_a: op.hilo_a(),
                high_b: op.hilo_b(),
                psl: false,
                merge: false,
                extended: op.x(),
                write_cc: op.write_cc(),
            };
            emit_xmad2(ctx, op.xmad_cop(), a, b, c, op.dest(), f);
        }
        _ => return None,
    }
    Some(Ok(()))
}
