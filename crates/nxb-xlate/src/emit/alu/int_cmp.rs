//! `InstEmitIntegerComparison.cs`, `InstEmitIntegerMinMax.cs`.

use super::helper::*;
use crate::emit::emitter::EmitterContext;
use crate::emit::helper::*;
use crate::ir::inst::Inst;
use crate::ir::ops::{FALSE, Opd, TRUE, regtype};
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

fn get_int_comparison_extended(
    ctx: &mut Ctx,
    cond: IComp,
    a: Opd,
    b: Opd,
    signed: bool,
) -> Result<Opd, String> {
    if cond == IComp::T {
        return Ok(ctx.ir.const_(TRUE));
    }
    if cond == IComp::F {
        return Ok(ctx.ir.const_(FALSE));
    }
    let res = ctx.i_subtract(a, b);
    let cf = get_cf(ctx);
    let ncf = ctx.bitwise_not(cf);
    let _ = ctx.i_add(res, ncf);
    Ok(match cond {
        IComp::Eq => {
            let e = ctx.i_compare_equal(a, b);
            let zf = get_zf(ctx);
            ctx.bitwise_and(e, zf)
        }
        IComp::Lt => {
            let cf = get_cf(ctx);
            let not_c = ctx.bitwise_not(cf);
            let e = ctx.i_compare_equal(a, b);
            let prev = ctx.bitwise_and(e, not_c);
            let l = if signed {
                ctx.i_compare_less(a, b)
            } else {
                ctx.i_compare_less_unsigned(a, b)
            };
            ctx.bitwise_or(l, prev)
        }
        IComp::Le => {
            let zf = get_zf(ctx);
            let cf = get_cf(ctx);
            let ncf = ctx.bitwise_not(cf);
            let z_or_not_c = ctx.bitwise_or(zf, ncf);
            let e = ctx.i_compare_equal(a, b);
            let prev = ctx.bitwise_and(e, z_or_not_c);
            let l = if signed {
                ctx.i_compare_less(a, b)
            } else {
                ctx.i_compare_less_unsigned(a, b)
            };
            ctx.bitwise_or(l, prev)
        }
        IComp::Gt => {
            let zf = get_zf(ctx);
            let nz = ctx.bitwise_not(zf);
            let cf = get_cf(ctx);
            let not_z_and_c = ctx.bitwise_and(nz, cf);
            let e = ctx.i_compare_equal(a, b);
            let prev = ctx.bitwise_and(e, not_z_and_c);
            let g = if signed {
                ctx.i_compare_greater(a, b)
            } else {
                ctx.i_compare_greater_unsigned(a, b)
            };
            ctx.bitwise_or(g, prev)
        }
        IComp::Ge => {
            let e = ctx.i_compare_equal(a, b);
            let cf = get_cf(ctx);
            let prev = ctx.bitwise_and(e, cf);
            let g = if signed {
                ctx.i_compare_greater(a, b)
            } else {
                ctx.i_compare_greater_unsigned(a, b)
            };
            ctx.bitwise_or(g, prev)
        }
        IComp::Ne => {
            let n = ctx.i_compare_not_equal(a, b);
            let zf = get_zf(ctx);
            let nz = ctx.bitwise_not(zf);
            ctx.bitwise_or(n, nz)
        }
        c => return Err(format!("Unexpected condition \"{}\".", c.0)),
    })
}

fn get_int_comparison_plain(
    ctx: &mut Ctx,
    cond: IComp,
    a: Opd,
    b: Opd,
    signed: bool,
) -> Result<Opd, String> {
    if cond == IComp::T {
        return Ok(ctx.ir.const_(TRUE));
    }
    if cond == IComp::F {
        return Ok(ctx.ir.const_(FALSE));
    }
    let mut inst = match cond {
        IComp::Lt => Inst::CompareLessU32,
        IComp::Eq => Inst::CompareEqual,
        IComp::Le => Inst::CompareLessOrEqualU32,
        IComp::Gt => Inst::CompareGreaterU32,
        IComp::Ne => Inst::CompareNotEqual,
        IComp::Ge => Inst::CompareGreaterOrEqualU32,
        c => return Err(format!("Unexpected condition \"{}\".", c.0)),
    };
    if signed {
        match cond {
            IComp::Lt => inst = Inst::CompareLess,
            IComp::Le => inst = Inst::CompareLessOrEqual,
            IComp::Gt => inst = Inst::CompareGreater,
            IComp::Ge => inst = Inst::CompareGreaterOrEqual,
            _ => {}
        }
    }
    let dest = ctx.ir.local();
    ctx.add(inst, Some(dest), &[a, b]);
    Ok(dest)
}

/// `GetIntComparison` (with `extended`).
pub(crate) fn get_int_comparison(
    ctx: &mut Ctx,
    cond: IComp,
    a: Opd,
    b: Opd,
    signed: bool,
    extended: bool,
) -> Result<Opd, String> {
    if extended {
        get_int_comparison_extended(ctx, cond, a, b, signed)
    } else {
        get_int_comparison_plain(ctx, cond, a, b, signed)
    }
}

fn emit_icmp(ctx: &mut Ctx, cmp: IComp, a: Opd, b: Opd, c: Opd, rd: i32, signed: bool) -> R {
    let z = ctx.ir.const_(0);
    let cmp_res = get_int_comparison(ctx, cmp, c, z, signed, false)?;
    let res = ctx.conditional_select(cmp_res, a, b);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
    Ok(())
}

struct IsetArgs {
    cmp: IComp,
    logic: BoolOp,
    src_pred: i32,
    src_pred_inv: bool,
    rd: i32,
    bool_float: bool,
    signed: bool,
    extended: bool,
    write_cc: bool,
}

fn emit_iset(ctx: &mut Ctx, a: Opd, b: Opd, p: IsetArgs) -> R {
    let mut res = get_int_comparison(ctx, p.cmp, a, b, p.signed, p.extended)?;
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
        set_zn_flags(ctx, res, p.write_cc, p.extended);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_isetp(
    ctx: &mut Ctx,
    cmp: IComp,
    logic: BoolOp,
    a: Opd,
    b: Opd,
    src_pred: i32,
    src_pred_inv: bool,
    dest_pred: i32,
    dest_pred_inv: i32,
    signed: bool,
    extended: bool,
) -> R {
    let p0 = get_int_comparison(ctx, cmp, a, b, signed, extended)?;
    let p1 = ctx.bitwise_not(p0);
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
fn emit_imnmx(ctx: &mut Ctx, a: Opd, b: Opd, src_pred: Opd, rd: i32, signed: bool, wcc: bool) {
    let rmin = if signed {
        ctx.i_minimum_s32(a, b)
    } else {
        ctx.i_minimum_u32(a, b)
    };
    let rmax = if signed {
        ctx.i_maximum_s32(a, b)
    } else {
        ctx.i_maximum_u32(a, b)
    };
    let res = ctx.conditional_select(src_pred, rmin, rmax);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, res);
    set_zn_flags(ctx, res, wcc, false);
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    macro_rules! icmp {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let c = get_src_reg(ctx, op.src_c(), false);
            emit_icmp(ctx, op.i_comp(), a, b, c, op.dest(), op.signed())
        }};
    }
    macro_rules! iset {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_iset(
                ctx,
                a,
                b,
                IsetArgs {
                    cmp: op.i_comp(),
                    logic: op.bop(),
                    src_pred: op.src_pred(),
                    src_pred_inv: op.src_pred_inv(),
                    rd: op.dest(),
                    bool_float: op.b_val(),
                    signed: op.signed(),
                    extended: op.x(),
                    write_cc: op.write_cc(),
                },
            )
        }};
    }
    macro_rules! isetp {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            emit_isetp(
                ctx,
                op.i_comp(),
                op.bop(),
                a,
                b,
                op.src_pred(),
                op.src_pred_inv(),
                op.dest_pred(),
                op.dest_pred_inv(),
                op.signed(),
                op.x(),
            )
        }};
    }
    macro_rules! imnmx {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = src_b!($k, ctx, op);
            let p = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
            emit_imnmx(ctx, a, b, p, op.dest(), op.signed(), op.write_cc());
            Ok(())
        }};
    }
    Some(match emit {
        Emit::IcmpR => icmp!(InstIcmpR, R),
        Emit::IcmpI => icmp!(InstIcmpI, S),
        Emit::IcmpC => icmp!(InstIcmpC, C),
        Emit::IcmpRc => {
            let op = InstIcmpRc(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_c(), false);
            let c = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            emit_icmp(ctx, op.i_comp(), a, b, c, op.dest(), op.signed())
        }
        Emit::IsetR => iset!(InstIsetR, R),
        Emit::IsetI => iset!(InstIsetI, S),
        Emit::IsetC => iset!(InstIsetC, C),
        Emit::IsetpR => isetp!(InstIsetpR, R),
        Emit::IsetpI => isetp!(InstIsetpI, S),
        Emit::IsetpC => isetp!(InstIsetpC, C),
        Emit::ImnmxR => imnmx!(InstImnmxR, R),
        Emit::ImnmxI => imnmx!(InstImnmxI, S),
        Emit::ImnmxC => imnmx!(InstImnmxC, C),
        _ => return None,
    })
}
