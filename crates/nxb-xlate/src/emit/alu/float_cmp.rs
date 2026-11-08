//! `InstEmitFloatComparison.cs`, `InstEmitFloatMinMax.cs`.

use super::helper::*;
use crate::emit::helper::*;
use crate::ir::emitter::{EmitterContext, FP32, FP64};
use crate::ir::inst::Inst;
use crate::ir::ops::{Opd, regtype};
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

fn emit_fcmp(ctx: &mut Ctx, cmp: FComp, a: Opd, b: Opd, c: Opd, rd: i32, ftz: bool) -> R {
    let z = ctx.ir.const_f(0.0);
    let c = if ftz { flush_denormal(ctx, c) } else { c };
    let cmp_res = get_fp_comparison(ctx, cmp, c, z, FP32)?;
    let res = ctx.conditional_select(cmp_res, a, b);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
    Ok(())
}

struct FsetArgs {
    cmp: FComp,
    logic: BoolOp,
    src_pred: i32,
    src_pred_inv: bool,
    rd: i32,
    abs_a: bool,
    abs_b: bool,
    neg_a: bool,
    neg_b: bool,
    bool_float: bool,
    write_cc: bool,
    is64: bool,
    ftz: bool,
}

fn emit_fset(ctx: &mut Ctx, a: Opd, b: Opd, p: FsetArgs) -> R {
    let fp = if p.is64 { FP64 } else { FP32 };
    let a = ctx.fp_abs_neg_t(a, p.abs_a, p.neg_a, fp);
    let b = ctx.fp_abs_neg_t(b, p.abs_b, p.neg_b, fp);
    let (a, b) = if p.ftz {
        flush_denormal_pair(ctx, a, b)
    } else {
        (a, b)
    };
    let mut res = get_fp_comparison(ctx, p.cmp, a, b, fp)?;
    let pred = get_predicate(ctx, p.src_pred, p.src_pred_inv);
    res = get_pred_logical_op(ctx, p.logic, res, pred);
    let dest = get_dest(ctx, p.rd);
    if p.bool_float {
        let one = ctx.ir.const_f(1.0);
        let zero = ctx.ir.const_(0);
        res = ctx.conditional_select(res, one, zero);
        ctx.copy_to(dest, res);
        set_fp_zn_flags32(ctx, res, p.write_cc);
    } else {
        ctx.copy_to(dest, res);
        set_zn_flags(ctx, res, p.write_cc, false);
    }
    Ok(())
}

struct FsetpArgs {
    cmp: FComp,
    logic: BoolOp,
    src_pred: i32,
    src_pred_inv: bool,
    dest_pred: i32,
    dest_pred_inv: i32,
    abs_a: bool,
    abs_b: bool,
    neg_a: bool,
    neg_b: bool,
    is64: bool,
    ftz: bool,
}

fn emit_fsetp(ctx: &mut Ctx, a: Opd, b: Opd, p: FsetpArgs) -> R {
    let fp = if p.is64 { FP64 } else { FP32 };
    let a = ctx.fp_abs_neg_t(a, p.abs_a, p.neg_a, fp);
    let b = ctx.fp_abs_neg_t(b, p.abs_b, p.neg_b, fp);
    let (a, b) = if p.ftz {
        flush_denormal_pair(ctx, a, b)
    } else {
        (a, b)
    };
    let p0 = get_fp_comparison(ctx, p.cmp, a, b, fp)?;
    let p1 = ctx.bitwise_not(p0);
    let pred = get_predicate(ctx, p.src_pred, p.src_pred_inv);
    let p0 = get_pred_logical_op(ctx, p.logic, p0, pred);
    let p1 = get_pred_logical_op(ctx, p.logic, p1, pred);
    let d0 = ctx.ir.register(p.dest_pred, regtype::PREDICATE);
    ctx.copy_to(d0, p0);
    let d1 = ctx.ir.register(p.dest_pred_inv, regtype::PREDICATE);
    ctx.copy_to(d1, p1);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_hset2(
    ctx: &mut Ctx,
    cmp: FComp,
    logic: BoolOp,
    a: [Opd; 2],
    b: [Opd; 2],
    src_pred: i32,
    src_pred_inv: bool,
    rd: i32,
    bool_float: bool,
) -> R {
    let r0 = get_fp_comparison(ctx, cmp, a[0], b[0], FP32)?;
    let r1 = get_fp_comparison(ctx, cmp, a[1], b[1], FP32)?;
    let pred = get_predicate(ctx, src_pred, src_pred_inv);
    let mut r0 = get_pred_logical_op(ctx, logic, r0, pred);
    let mut r1 = get_pred_logical_op(ctx, logic, r1, pred);
    if bool_float {
        let one = ctx.ir.const_f(1.0);
        let zero = ctx.ir.const_(0);
        r0 = ctx.conditional_select(r0, one, zero);
        let one = ctx.ir.const_f(1.0);
        let zero = ctx.ir.const_(0);
        r1 = ctx.conditional_select(r1, one, zero);
        let d = get_dest(ctx, rd);
        let p = ctx.pack_half2x16(r0, r1);
        ctx.copy_to(d, p);
    } else {
        let m = ctx.ir.const_(0xffff);
        let low = ctx.bitwise_and(r0, m);
        let s = ctx.ir.const_(16);
        let high = ctx.shift_left(r1, s);
        let packed = ctx.bitwise_or(low, high);
        let d = get_dest(ctx, rd);
        ctx.copy_to(d, packed);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_hsetp2(
    ctx: &mut Ctx,
    cmp: FComp,
    logic: BoolOp,
    a: [Opd; 2],
    b: [Opd; 2],
    src_pred: i32,
    src_pred_inv: bool,
    dest_pred: i32,
    dest_pred_inv: i32,
    h_and: bool,
) -> R {
    let mut p0 = get_fp_comparison(ctx, cmp, a[0], b[0], FP32)?;
    let mut p1 = get_fp_comparison(ctx, cmp, a[1], b[1], FP32)?;
    if h_and {
        p0 = ctx.bitwise_and(p0, p1);
        p1 = ctx.bitwise_not(p0);
    }
    let pred = get_predicate(ctx, src_pred, src_pred_inv);
    let p0 = get_pred_logical_op(ctx, logic, p0, pred);
    let p1 = get_pred_logical_op(ctx, logic, p1, pred);
    let d0 = ctx.ir.register(dest_pred, regtype::PREDICATE);
    ctx.copy_to(d0, p0);
    let d1 = ctx.ir.register(dest_pred_inv, regtype::PREDICATE);
    ctx.copy_to(d1, p1);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_fmnmx(
    ctx: &mut Ctx,
    a: Opd,
    b: Opd,
    src_pred: Opd,
    rd: i32,
    abs_a: bool,
    abs_b: bool,
    neg_a: bool,
    neg_b: bool,
    write_cc: bool,
    is64: bool,
) {
    let fp: Inst = if is64 { FP64 } else { FP32 };
    let a = ctx.fp_abs_neg_t(a, abs_a, neg_a, fp);
    let b = ctx.fp_abs_neg_t(b, abs_b, neg_b, fp);
    let rmin = ctx.fp_minimum_t(a, b, fp);
    let rmax = ctx.fp_maximum_t(a, b, fp);
    let res = ctx.conditional_select(src_pred, rmin, rmax);
    set_dest(ctx, res, rd, is64);
    set_fp_zn_flags(ctx, res, write_cc, fp);
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    macro_rules! fset {
        ($ty:ident, $is64:expr, $b:expr, $ftz:expr) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), $is64);
            let b = $b(ctx, &op);
            emit_fset(
                ctx,
                a,
                b,
                FsetArgs {
                    cmp: op.f_comp(),
                    logic: op.bop(),
                    src_pred: op.src_pred(),
                    src_pred_inv: op.src_pred_inv(),
                    rd: op.dest(),
                    abs_a: op.abs_a(),
                    abs_b: op.abs_b(),
                    neg_a: op.neg_a(),
                    neg_b: op.neg_b(),
                    bool_float: op.b_val(),
                    write_cc: op.write_cc(),
                    is64: $is64,
                    ftz: ($ftz)(&op),
                },
            )
        }};
    }
    macro_rules! fsetp {
        ($ty:ident, $is64:expr, $wcc:expr, $b:expr, $ftz:expr) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), $is64);
            let b = $b(ctx, &op);
            let _ = $wcc;
            emit_fsetp(
                ctx,
                a,
                b,
                FsetpArgs {
                    cmp: op.f_comp(),
                    logic: op.bop(),
                    src_pred: op.src_pred(),
                    src_pred_inv: op.src_pred_inv(),
                    dest_pred: op.dest_pred(),
                    dest_pred_inv: op.dest_pred_inv(),
                    abs_a: op.abs_a(),
                    abs_b: op.abs_b(),
                    neg_a: op.neg_a(),
                    neg_b: op.neg_b(),
                    is64: $is64,
                    ftz: ($ftz)(&op),
                },
            )
        }};
    }
    macro_rules! fcmp {
        ($ty:ident, $b:expr, $c:expr) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = $b(ctx, &op);
            let c = $c(ctx, &op);
            emit_fcmp(ctx, op.f_comp(), a, b, c, op.dest(), op.ftz())
        }};
    }
    macro_rules! fmnmx {
        ($ty:ident, $is64:expr, $b:expr) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), $is64);
            let b = $b(ctx, &op);
            let p = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
            emit_fmnmx(
                ctx,
                a,
                b,
                p,
                op.dest(),
                op.abs_a(),
                op.abs_b(),
                op.neg_a(),
                op.neg_b(),
                op.write_cc(),
                $is64,
            );
            Ok(())
        }};
    }
    let r: R = match emit {
        Emit::DsetR => fset!(
            InstDsetR,
            true,
            |c: &mut Ctx, o: &InstDsetR| get_src_reg(c, o.src_b(), true),
            |_: &InstDsetR| false
        ),
        Emit::DsetI => fset!(
            InstDsetI,
            true,
            |c: &mut Ctx, o: &InstDsetI| get_src_imm(c, imm20_to_float(o.imm20()), true),
            |_: &InstDsetI| false
        ),
        Emit::DsetC => fset!(
            InstDsetC,
            true,
            |c: &mut Ctx, o: &InstDsetC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), true),
            |_: &InstDsetC| false
        ),
        Emit::FsetR => fset!(
            InstFsetR,
            false,
            |c: &mut Ctx, o: &InstFsetR| get_src_reg(c, o.src_b(), false),
            |o: &InstFsetR| o.ftz()
        ),
        Emit::FsetC => fset!(
            InstFsetC,
            false,
            |c: &mut Ctx, o: &InstFsetC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), false),
            |o: &InstFsetC| o.ftz()
        ),
        Emit::FsetI => fset!(
            InstFsetI,
            false,
            |c: &mut Ctx, o: &InstFsetI| get_src_imm(c, imm20_to_float(o.imm20()), false),
            |o: &InstFsetI| o.ftz()
        ),
        Emit::DsetpR => fsetp!(
            InstDsetpR,
            true,
            false,
            |c: &mut Ctx, o: &InstDsetpR| { get_src_reg(c, o.src_b(), true) },
            |_: &InstDsetpR| false
        ),
        Emit::DsetpI => fsetp!(
            InstDsetpI,
            true,
            false,
            |c: &mut Ctx, o: &InstDsetpI| { get_src_imm(c, imm20_to_float(o.imm20()), true) },
            |_: &InstDsetpI| false
        ),
        Emit::DsetpC => fsetp!(
            InstDsetpC,
            true,
            false,
            |c: &mut Ctx, o: &InstDsetpC| { get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), true) },
            |_: &InstDsetpC| false
        ),
        Emit::FsetpR => fsetp!(
            InstFsetpR,
            false,
            0,
            |c: &mut Ctx, o: &InstFsetpR| { get_src_reg(c, o.src_b(), false) },
            |o: &InstFsetpR| o.ftz()
        ),
        Emit::FsetpI => fsetp!(
            InstFsetpI,
            false,
            0,
            |c: &mut Ctx, o: &InstFsetpI| { get_src_imm(c, imm20_to_float(o.imm20()), false) },
            |o: &InstFsetpI| o.ftz()
        ),
        Emit::FsetpC => fsetp!(
            InstFsetpC,
            false,
            0,
            |c: &mut Ctx, o: &InstFsetpC| {
                get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), false)
            },
            |o: &InstFsetpC| o.ftz()
        ),
        Emit::FcmpR => fcmp!(
            InstFcmpR,
            |c: &mut Ctx, o: &InstFcmpR| get_src_reg(c, o.src_b(), false),
            |c: &mut Ctx, o: &InstFcmpR| get_src_reg(c, o.src_c(), false)
        ),
        Emit::FcmpI => fcmp!(
            InstFcmpI,
            |c: &mut Ctx, o: &InstFcmpI| get_src_imm(c, imm20_to_float(o.imm20()), false),
            |c: &mut Ctx, o: &InstFcmpI| get_src_reg(c, o.src_c(), false)
        ),
        Emit::FcmpC => fcmp!(
            InstFcmpC,
            |c: &mut Ctx, o: &InstFcmpC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), false),
            |c: &mut Ctx, o: &InstFcmpC| get_src_reg(c, o.src_c(), false)
        ),
        Emit::FcmpRc => fcmp!(
            InstFcmpRc,
            |c: &mut Ctx, o: &InstFcmpRc| get_src_reg(c, o.src_c(), false),
            |c: &mut Ctx, o: &InstFcmpRc| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), false)
        ),
        Emit::DmnmxR => fmnmx!(InstDmnmxR, true, |c: &mut Ctx, o: &InstDmnmxR| get_src_reg(
            c,
            o.src_b(),
            true
        )),
        Emit::DmnmxI => fmnmx!(InstDmnmxI, true, |c: &mut Ctx, o: &InstDmnmxI| get_src_imm(
            c,
            imm20_to_float(o.imm20()),
            true
        )),
        Emit::DmnmxC => fmnmx!(
            InstDmnmxC,
            true,
            |c: &mut Ctx, o: &InstDmnmxC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), true)
        ),
        Emit::FmnmxR => fmnmx!(
            InstFmnmxR,
            false,
            |c: &mut Ctx, o: &InstFmnmxR| get_src_reg(c, o.src_b(), false)
        ),
        Emit::FmnmxI => fmnmx!(
            InstFmnmxI,
            false,
            |c: &mut Ctx, o: &InstFmnmxI| get_src_imm(c, imm20_to_float(o.imm20()), false)
        ),
        Emit::FmnmxC => fmnmx!(InstFmnmxC, false, |c: &mut Ctx, o: &InstFmnmxC| {
            get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), false)
        }),
        Emit::Hset2R => {
            let op = InstHset2R(raw);
            (|| {
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_reg(ctx, op.b_swizzle(), op.src_b(), op.neg_b(), op.abs_b())?;
                emit_hset2(
                    ctx,
                    op.cmp(),
                    op.bop(),
                    a,
                    b,
                    op.src_pred(),
                    op.src_pred_inv(),
                    op.dest(),
                    op.bval(),
                )
            })()
        }
        Emit::Hset2I => {
            let op = InstHset2I(raw);
            (|| {
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_imm10(ctx, op.bimm_h0(), op.bimm_h1());
                emit_hset2(
                    ctx,
                    op.cmp(),
                    op.bop(),
                    a,
                    b,
                    op.src_pred(),
                    op.src_pred_inv(),
                    op.dest(),
                    op.bval(),
                )
            })()
        }
        Emit::Hset2C => {
            let op = InstHset2C(raw);
            (|| {
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_cbuf(
                    ctx,
                    HalfSwizzle::F32,
                    op.cbuf_slot(),
                    op.cbuf_offset(),
                    op.neg_b(),
                    false,
                )?;
                emit_hset2(
                    ctx,
                    op.cmp(),
                    op.bop(),
                    a,
                    b,
                    op.src_pred(),
                    op.src_pred_inv(),
                    op.dest(),
                    op.bval(),
                )
            })()
        }
        Emit::Hsetp2R => {
            let op = InstHsetp2R(raw);
            (|| {
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_reg(ctx, op.b_swizzle(), op.src_b(), op.neg_b(), op.abs_b())?;
                emit_hsetp2(
                    ctx,
                    op.f_comp2(),
                    op.bop(),
                    a,
                    b,
                    op.src_pred(),
                    op.src_pred_inv(),
                    op.dest_pred(),
                    op.dest_pred_inv(),
                    op.h_and(),
                )
            })()
        }
        Emit::Hsetp2I => {
            let op = InstHsetp2I(raw);
            (|| {
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_imm10(ctx, op.bimm_h0(), op.bimm_h1());
                emit_hsetp2(
                    ctx,
                    op.f_comp(),
                    op.bop(),
                    a,
                    b,
                    op.src_pred(),
                    op.src_pred_inv(),
                    op.dest_pred(),
                    op.dest_pred_inv(),
                    op.h_and(),
                )
            })()
        }
        Emit::Hsetp2C => {
            let op = InstHsetp2C(raw);
            (|| {
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_cbuf(
                    ctx,
                    HalfSwizzle::F32,
                    op.cbuf_slot(),
                    op.cbuf_offset(),
                    op.neg_b(),
                    op.abs_b(),
                )?;
                emit_hsetp2(
                    ctx,
                    op.f_comp(),
                    op.bop(),
                    a,
                    b,
                    op.src_pred(),
                    op.src_pred_inv(),
                    op.dest_pred(),
                    op.dest_pred_inv(),
                    op.h_and(),
                )
            })()
        }
        _ => return None,
    };
    Some(r)
}
