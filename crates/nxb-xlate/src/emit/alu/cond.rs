//! `InstEmitConditionCode.cs`.

use super::helper::get_pred_logical_op;
use crate::emit::emitter::EmitterContext;
use crate::emit::helper::*;
use crate::ir::ops::{FALSE, Opd, TRUE, regtype};
use crate::maxwell::enums::Ccc;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

/// `GetCondition`.
pub(super) fn get_condition(ctx: &mut Ctx, cond: Ccc, default_cond: i32) -> Opd {
    macro_rules! f {
        (nf) => {
            get_nf(ctx)
        };
        (zf) => {
            get_zf(ctx)
        };
        (vf) => {
            get_vf(ctx)
        };
        (cf) => {
            get_cf(ctx)
        };
    }
    match cond {
        Ccc::F => ctx.ir.const_(FALSE),
        Ccc::Lt => {
            let nf = f!(nf);
            let zf = f!(zf);
            let nz = ctx.bitwise_not(zf);
            let a = ctx.bitwise_and(nf, nz);
            let vf = f!(vf);
            ctx.bitwise_exclusive_or(a, vf)
        }
        Ccc::Eq => {
            let nf = f!(nf);
            let n = ctx.bitwise_not(nf);
            let zf = f!(zf);
            ctx.bitwise_and(n, zf)
        }
        Ccc::Le => {
            let nf = f!(nf);
            let zf = f!(zf);
            let vf = f!(vf);
            let o = ctx.bitwise_or(zf, vf);
            ctx.bitwise_exclusive_or(nf, o)
        }
        Ccc::Gt => {
            let nf = f!(nf);
            let vf = f!(vf);
            let x = ctx.bitwise_exclusive_or(nf, vf);
            let zf = f!(zf);
            let o = ctx.bitwise_or(x, zf);
            ctx.bitwise_not(o)
        }
        Ccc::Ne => {
            let zf = f!(zf);
            ctx.bitwise_not(zf)
        }
        Ccc::Ge => {
            let nf = f!(nf);
            let vf = f!(vf);
            let x = ctx.bitwise_exclusive_or(nf, vf);
            ctx.bitwise_not(x)
        }
        Ccc::Num => {
            let nf = f!(nf);
            let zf = f!(zf);
            let a = ctx.bitwise_and(nf, zf);
            ctx.bitwise_not(a)
        }
        Ccc::Nan => {
            let nf = f!(nf);
            let zf = f!(zf);
            ctx.bitwise_and(nf, zf)
        }
        Ccc::Ltu => {
            let nf = f!(nf);
            let vf = f!(vf);
            ctx.bitwise_exclusive_or(nf, vf)
        }
        Ccc::Equ => f!(zf),
        Ccc::Leu => {
            let nf = f!(nf);
            let vf = f!(vf);
            let x = ctx.bitwise_exclusive_or(nf, vf);
            let zf = f!(zf);
            ctx.bitwise_or(x, zf)
        }
        Ccc::Gtu => {
            let nf = f!(nf);
            let n = ctx.bitwise_not(nf);
            let vf = f!(vf);
            let zf = f!(zf);
            let o = ctx.bitwise_or(vf, zf);
            ctx.bitwise_exclusive_or(n, o)
        }
        Ccc::Neu => {
            let nf = f!(nf);
            let zf = f!(zf);
            let n = ctx.bitwise_not(zf);
            ctx.bitwise_or(nf, n)
        }
        Ccc::Geu => {
            let nf = f!(nf);
            let n = ctx.bitwise_not(nf);
            let zf = f!(zf);
            let o = ctx.bitwise_or(n, zf);
            let vf = f!(vf);
            ctx.bitwise_exclusive_or(o, vf)
        }
        Ccc::T => ctx.ir.const_(TRUE),
        Ccc::Off => {
            let vf = f!(vf);
            ctx.bitwise_not(vf)
        }
        Ccc::Lo => {
            let cf = f!(cf);
            ctx.bitwise_not(cf)
        }
        Ccc::Sff => {
            let nf = f!(nf);
            ctx.bitwise_not(nf)
        }
        Ccc::Ls => {
            let zf = f!(zf);
            let cf = f!(cf);
            let n = ctx.bitwise_not(cf);
            ctx.bitwise_or(zf, n)
        }
        Ccc::Hi => {
            let cf = f!(cf);
            let zf = f!(zf);
            let n = ctx.bitwise_not(zf);
            ctx.bitwise_and(cf, n)
        }
        Ccc::Sft => f!(nf),
        Ccc::Hs => f!(cf),
        Ccc::Oft => f!(vf),
        Ccc::Rle => {
            let nf = f!(nf);
            let zf = f!(zf);
            ctx.bitwise_or(nf, zf)
        }
        Ccc::Rgt => {
            let nf = f!(nf);
            let zf = f!(zf);
            let o = ctx.bitwise_or(nf, zf);
            ctx.bitwise_not(o)
        }
        _ => ctx.ir.const_(default_cond),
    }
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    match emit {
        Emit::Cset => {
            let op = InstCset(raw);
            let mut res = get_condition(ctx, op.ccc(), TRUE);
            let sp = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
            res = get_pred_logical_op(ctx, op.bop(), res, sp);
            let dest = get_dest(ctx, op.dest());
            if op.b_val() {
                let one = ctx.ir.const_f(1.0);
                let zero = ctx.ir.const_(0);
                let s = ctx.conditional_select(res, one, zero);
                ctx.copy_to(dest, s);
            } else {
                ctx.copy_to(dest, res);
            }
        }
        Emit::Csetp => {
            let op = InstCsetp(raw);
            let p0 = get_condition(ctx, op.ccc(), TRUE);
            let p1 = ctx.bitwise_not(p0);
            let sp = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
            let p0 = get_pred_logical_op(ctx, op.bop(), p0, sp);
            let p1 = get_pred_logical_op(ctx, op.bop(), p1, sp);
            let d0 = ctx.ir.register(op.dest_pred(), regtype::PREDICATE);
            ctx.copy_to(d0, p0);
            let d1 = ctx.ir.register(op.dest_pred_inv(), regtype::PREDICATE);
            ctx.copy_to(d1, p1);
        }
        _ => return None,
    }
    Some(Ok(()))
}
