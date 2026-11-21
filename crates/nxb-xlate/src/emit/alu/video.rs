//! `InstEmitVideoArithmetic.cs`, `InstEmitVideoMinMax.cs`, `InstEmitWarp.cs`.

use super::helper::*;
use super::int_cmp::get_int_comparison;
use crate::emit::emitter::EmitterContext;
use crate::emit::helper::*;
use crate::ir::ops::{Opd, regtype};
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;
use crate::translation::host;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

const S8B0: i32 = 8;

fn video_src_b(ctx: &mut Ctx, b_video: bool, src_b: i32, sel: VectorSelect, imm16: i32) -> Opd {
    if b_video {
        let s = get_src_reg(ctx, src_b, false);
        extend(ctx, s, sel)
    } else {
        let mut imm = imm16;
        if sel.0 & S8B0 != 0 {
            imm = (imm << 16) >> 16;
        }
        ctx.ir.const_(imm)
    }
}

fn vmnmx(ctx: &mut Ctx) {
    let op = InstVmnmx(ctx.curr_op.raw);
    let s = get_src_reg(ctx, op.src_a(), false);
    let src_a = extend(ctx, s, op.a_select());
    let src_c = get_src_reg(ctx, op.src_c(), false);
    let src_b = video_src_b(ctx, op.b_video(), op.src_b(), op.b_select(), op.imm16());
    let mut res;
    let res_signed;
    if (op.a_select().0 & S8B0) != (op.b_select().0 & S8B0) {
        res_signed = !op.mn();
        res = if op.mn() {
            ctx.i_maximum_u32(src_a, src_b)
        } else {
            ctx.i_minimum_s32(src_a, src_b)
        };
        if op.a_select().0 & S8B0 != 0 {
            let z = ctx.ir.const_(0);
            let c = ctx.i_compare_less(src_b, z);
            res = ctx.conditional_select(c, src_b, res);
        } else {
            let z = ctx.ir.const_(0);
            let c = ctx.i_compare_less(src_a, z);
            res = ctx.conditional_select(c, src_a, res);
        }
    } else {
        res_signed = op.a_select().0 & S8B0 != 0;
        res = match (op.mn(), res_signed) {
            (true, true) => ctx.i_maximum_s32(src_a, src_b),
            (true, false) => ctx.i_maximum_u32(src_a, src_b),
            (false, true) => ctx.i_minimum_s32(src_a, src_b),
            (false, false) => ctx.i_minimum_u32(src_a, src_b),
        };
    }
    if op.sat() {
        if op.d_format() && !res_signed {
            let m = ctx.ir.const_(i32::MAX);
            res = ctx.i_minimum_u32(res, m);
        } else if !op.d_format() && res_signed {
            let z = ctx.ir.const_(0);
            res = ctx.i_maximum_s32(res, z);
        }
    }
    match op.video_op() {
        VideoOp::Acc => res = ctx.i_add(res, src_c),
        VideoOp::Max => {
            res = if op.d_format() {
                ctx.i_maximum_s32(res, src_c)
            } else {
                ctx.i_maximum_u32(res, src_c)
            }
        }
        VideoOp::Min => {
            res = if op.d_format() {
                ctx.i_minimum_s32(res, src_c)
            } else {
                ctx.i_minimum_u32(res, src_c)
            }
        }
        VideoOp::Mrg16h => res = merge(ctx, src_c, res, 16, 16),
        VideoOp::Mrg16l => res = merge(ctx, src_c, res, 0, 16),
        VideoOp::Mrg8b0 => res = merge(ctx, src_c, res, 0, 8),
        VideoOp::Mrg8b2 => res = merge(ctx, src_c, res, 16, 8),
        _ => {}
    }
    let d = get_dest(ctx, op.dest());
    ctx.copy_to(d, res);
}

fn merge(ctx: &mut Ctx, base: Opd, v: Opd, pos: i32, size: i32) -> Opd {
    let p = ctx.ir.const_(pos);
    let s = ctx.ir.const_(size);
    ctx.bitfield_insert(base, v, p, s)
}

fn vsetp(ctx: &mut Ctx) -> R {
    let op = InstVsetp(ctx.curr_op.raw);
    let s = get_src_reg(ctx, op.src_a(), false);
    let src_a = extend(ctx, s, op.a_select());
    let src_b = video_src_b(ctx, op.b_video(), op.src_b(), op.b_select(), op.imm16());
    let signed_a = op.a_select().0 & S8B0 != 0;
    let signed_b = op.b_select().0 & S8B0 != 0;
    let p0;
    if signed_a != signed_b {
        let a32 = (op.a_select().0 & !S8B0) == VectorSelect::U32.0;
        let b32 = (op.b_select().0 & !S8B0) == VectorSelect::U32.0;
        if !a32 && !b32 {
            p0 = get_int_comparison(ctx, op.v_comp(), src_a, src_b, true, false)?;
        } else {
            p0 = ctx.ir.const_(0);
        }
    } else {
        p0 = get_int_comparison(ctx, op.v_comp(), src_a, src_b, signed_a, false)?;
    }
    let p1 = ctx.bitwise_not(p0);
    let pred = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
    let p0 = get_pred_logical_op(ctx, op.bool_op(), p0, pred);
    let p1 = get_pred_logical_op(ctx, op.bool_op(), p1, pred);
    let d0 = ctx.ir.register(op.dest_pred(), regtype::PREDICATE);
    ctx.copy_to(d0, p0);
    let d1 = ctx.ir.register(op.dest_pred_inv(), regtype::PREDICATE);
    ctx.copy_to(d1, p1);
    Ok(())
}

fn vmad(ctx: &mut Ctx) {
    let op = InstVmad(ctx.curr_op.raw);
    let a_signed = op.a_select().0 & S8B0 != 0;
    let b_signed = op.b_select().0 & S8B0 != 0;
    let s = get_src_reg(ctx, op.src_a(), false);
    let src_a = extend(ctx, s, op.a_select());
    let c0 = get_src_reg(ctx, op.src_c(), false);
    let src_c = ctx.i_negate_if(c0, op.avg_mode() == AvgMode::NegB);
    let src_b = if op.b_video() {
        let s = get_src_reg(ctx, op.src_b(), false);
        extend(ctx, s, op.b_select())
    } else {
        let mut imm = op.imm16();
        if b_signed {
            imm = (imm << 16) >> 16;
        }
        ctx.ir.const_(imm)
    };
    let mut low = ctx.i_multiply(src_a, src_b);
    let mut high;
    if a_signed == b_signed {
        high = if a_signed {
            ctx.multiply_high_s32(src_a, src_b)
        } else {
            ctx.multiply_high_u32(src_a, src_b)
        };
    } else {
        let temp = if a_signed {
            let k = ctx.ir.const_(31);
            let sh = ctx.shift_right_s32(src_a, k);
            ctx.i_multiply(src_b, sh)
        } else {
            let k = ctx.ir.const_(31);
            let sh = ctx.shift_right_s32(src_b, k);
            ctx.i_multiply(src_a, sh)
        };
        let mh = ctx.multiply_high_u32(src_a, src_b);
        high = ctx.i_add(temp, mh);
    }
    if op.avg_mode() == AvgMode::NegA {
        (low, high) = negate_long(ctx, low, high);
    }
    let (mut res_low, sum_carry) = add_with_carry(ctx, low, src_c);
    let mut res_high = ctx.i_add(high, sum_carry);
    if op.avg_mode() == AvgMode::PlusOne {
        let one = ctx.ir.const_(1);
        let (l, c) = add_with_carry(ctx, res_low, one);
        res_low = l;
        res_high = ctx.i_add(res_high, c);
    }
    let res_signed = op.a_select() == VectorSelect::S32
        || op.b_select() == VectorSelect::S32
        || op.avg_mode() == AvgMode::NegB
        || op.avg_mode() == AvgMode::NegA;
    let shift = match op.video_scale() {
        VideoScale::Shr7 => 7,
        VideoScale::Shr15 => 15,
        _ => 0,
    };
    if shift != 0 {
        let k = ctx.ir.const_(shift);
        res_low = ctx.shift_right_u32(res_low, k);
        let k = ctx.ir.const_(32 - shift);
        let hs = ctx.shift_left(res_high, k);
        res_low = ctx.bitwise_or(res_low, hs);
        let k = ctx.ir.const_(shift);
        res_high = if res_signed {
            ctx.shift_right_s32(res_high, k)
        } else {
            ctx.shift_right_u32(res_high, k)
        };
    }
    let mut res = res_low;
    if op.sat() {
        let k = ctx.ir.const_(31);
        let sign = ctx.shift_right_s32(res_high, k);
        if res_signed {
            let k = ctx.ir.const_(31);
            let lo_sign = ctx.shift_right_s32(res_low, k);
            let overflow = ctx.i_compare_not_equal(res_high, lo_sign);
            let mn = ctx.ir.const_(i32::MIN);
            let mx = ctx.ir.const_(i32::MAX);
            let clamp = ctx.conditional_select(sign, mn, mx);
            res = ctx.conditional_select(overflow, clamp, res_low);
        } else {
            let z = ctx.ir.const_(0);
            let overflow = ctx.i_compare_not_equal(res_high, z);
            let ns = ctx.bitwise_not(sign);
            res = ctx.conditional_select(overflow, ns, res_low);
        }
    }
    let d = get_dest(ctx, op.dest());
    ctx.copy_to(d, res);
}

pub(crate) fn emit_ballot(ctx: &mut Ctx, pred: Opd) -> Result<Opd, String> {
    if host::SUBGROUP_SIZE <= 32 {
        Ok(ctx.ballot(pred, 0))
    } else {
        Err("unported: Ballot for a host subgroup size above 32".into())
    }
}

pub(crate) fn emit_vote(ctx: &mut Ctx, mode: VoteMode, pred: Opd) -> Result<Option<Opd>, String> {
    if host::SUBGROUP_SIZE > 32 {
        return Err("unported: Vote for a host subgroup size above 32".into());
    }
    Ok(match mode {
        VoteMode::All => Some(ctx.vote_all(pred)),
        VoteMode::Any => Some(ctx.vote_any(pred)),
        VoteMode::Eq => Some(ctx.vote_all_equal(pred)),
        _ => None,
    })
}

fn vote(ctx: &mut Ctx) -> R {
    let op = InstVote(ctx.curr_op.raw);
    let pred = get_predicate(ctx, op.src_pred(), op.src_pred_inv());
    match emit_vote(ctx, op.vote_mode(), pred)? {
        Some(res) => {
            let d = ctx.ir.register(op.vp_dest(), regtype::PREDICATE);
            ctx.copy_to(d, res);
        }
        None => ctx
            .acc
            .log(&format!("Invalid vote operation: {}.", op.vote_mode().0)),
    }
    if op.dest() != 255 {
        let d = get_dest(ctx, op.dest());
        let b = emit_ballot(ctx, pred)?;
        ctx.copy_to(d, b);
    }
    Ok(())
}

fn shfl(ctx: &mut Ctx) -> R {
    let op = InstShfl(ctx.curr_op.raw);
    let pred = ctx.ir.register(op.dest_pred(), regtype::PREDICATE);
    let a = get_src_reg(ctx, op.src_a(), false);
    let b = if op.b_fix_shfl() {
        ctx.ir.const_(op.src_b_imm())
    } else {
        get_src_reg(ctx, op.src_b(), false)
    };
    let c = if op.c_fix_shfl() {
        ctx.ir.const_(op.src_c_imm())
    } else {
        get_src_reg(ctx, op.src_c(), false)
    };
    let (res, valid) = match op.shfl_mode() {
        ShflMode::Idx => ctx.shuffle_3(a, b, c),
        ShflMode::Up => ctx.shuffle_up_3(a, b, c),
        ShflMode::Down => ctx.shuffle_down_3(a, b, c),
        ShflMode::Bfly => ctx.shuffle_xor_3(a, b, c),
        m => return Err(format!("Invalid shuffle mode {}.", m.0)),
    };
    let d = get_dest(ctx, op.dest());
    ctx.copy_to(d, res);
    ctx.copy_to(pred, valid);
    Ok(())
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    Some(match emit {
        Emit::Vmnmx => {
            vmnmx(ctx);
            Ok(())
        }
        Emit::Vsetp => vsetp(ctx),
        Emit::Vmad => {
            vmad(ctx);
            Ok(())
        }
        Emit::Fswzadd => {
            let op = InstFswzadd(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = get_src_reg(ctx, op.src_b(), false);
            let dest = get_dest(ctx, op.dest());
            let r = ctx.fp_swizzle_add(a, b, op.pn_word());
            ctx.copy_to(dest, r);
            set_fp_zn_flags32(ctx, dest, op.write_cc());
            Ok(())
        }
        Emit::Shfl => shfl(ctx),
        Emit::Vote => vote(ctx),
        _ => return None,
    })
}
