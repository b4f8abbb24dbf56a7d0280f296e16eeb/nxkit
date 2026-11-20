//! `InstEmitConversion.cs`.

use super::helper::*;
use crate::emit::emitter::{EmitterContext, FP32, FP64};
use crate::emit::helper::*;
use crate::ir::inst::Inst;
use crate::ir::ops::Opd;
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

fn fp_type(t: DstFmt) -> Inst {
    if t == DstFmt::F64 { FP64 } else { FP32 }
}

fn invalid_fp(t: DstFmt) -> String {
    format!("Invalid floating point type \"{}\".", t.0)
}

fn half_pick(ctx: &mut Ctx, src: Opd, h: bool) -> Result<Opd, String> {
    let u = get_half_unpacked(ctx, src, HalfSwizzle::F16)?;
    Ok(u[usize::from(h)])
}

fn unpack_reg(ctx: &mut Ctx, t: DstFmt, h: bool, reg: i32) -> Result<Opd, String> {
    match t {
        DstFmt::F32 => Ok(get_src_reg(ctx, reg, false)),
        DstFmt::F16 => {
            let s = get_src_reg(ctx, reg, false);
            half_pick(ctx, s, h)
        }
        DstFmt::F64 => Ok(get_src_reg(ctx, reg, true)),
        _ => Err(invalid_fp(t)),
    }
}

fn unpack_cbuf(ctx: &mut Ctx, t: DstFmt, h: bool, slot: i32, off: i32) -> Result<Opd, String> {
    match t {
        DstFmt::F32 => Ok(get_src_cbuf(ctx, slot, off, false)),
        DstFmt::F16 => {
            let s = get_src_cbuf(ctx, slot, off, false);
            half_pick(ctx, s, h)
        }
        DstFmt::F64 => Ok(get_src_cbuf(ctx, slot, off, true)),
        _ => Err(invalid_fp(t)),
    }
}

fn unpack_imm(ctx: &mut Ctx, t: DstFmt, h: bool, imm: i32) -> Result<Opd, String> {
    match t {
        DstFmt::F32 => Ok(get_src_imm(ctx, imm, false)),
        DstFmt::F16 => {
            let s = get_src_imm(ctx, imm, false);
            half_pick(ctx, s, h)
        }
        DstFmt::F64 => Ok(get_src_imm(ctx, imm, true)),
        _ => Err(invalid_fp(t)),
    }
}

fn write_fp(ctx: &mut Ctx, t: DstFmt, src: Opd, rd: i32) {
    let dest = get_dest(ctx, rd);
    if t == DstFmt::F32 {
        ctx.copy_to(dest, src);
    } else if t == DstFmt::F16 {
        let z = ctx.ir.const_f(0.0);
        let p = ctx.pack_half2x16(src, z);
        ctx.copy_to(dest, p);
    } else {
        let dest2 = get_dest2(ctx, rd);
        let lo = ctx.unpack_double2x32_low(src);
        ctx.copy_to(dest, lo);
        let hi = ctx.unpack_double2x32_high(src);
        ctx.copy_to(dest2, hi);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_f2f(
    ctx: &mut Ctx,
    src_t: DstFmt,
    dst_t: DstFmt,
    round: IntegerRound,
    src: Opd,
    rd: i32,
    abs: bool,
    neg: bool,
    sat: bool,
) {
    let mut b = ctx.fp_abs_neg_t(src, abs, neg, fp_type(src_t));
    if src_t == dst_t {
        let t = fp_type(src_t);
        b = match round {
            IntegerRound::Round => ctx.fp_round_t(b, t),
            IntegerRound::Floor => ctx.fp_floor_t(b, t),
            IntegerRound::Ceil => ctx.fp_ceiling_t(b, t),
            IntegerRound::Trunc => ctx.fp_truncate_t(b, t),
            _ => b,
        };
    }
    if src_t == DstFmt::F32 && dst_t == DstFmt::F64 {
        b = ctx.fp32_convert_to_fp64(b);
    } else if src_t == DstFmt::F64 && dst_t == DstFmt::F32 {
        b = ctx.fp64_convert_to_fp32(b);
    }
    b = ctx.fp_saturate_if_t(b, sat, fp_type(dst_t));
    write_fp(ctx, dst_t, b, rd);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_f2i(
    ctx: &mut Ctx,
    src_t: DstFmt,
    dst_t: IDstFmt,
    round: RoundMode2,
    src: Opd,
    rd: i32,
    abs: bool,
    neg: bool,
) -> R {
    if dst_t == IDstFmt::U64 {
        ctx.acc.log("Unimplemented 64-bits F2I.");
    }
    let t = fp_type(src_t);
    let signed = dst_t == IDstFmt::S16 || dst_t == IDstFmt::S32 || dst_t == IDstFmt::S64;
    let small = dst_t == IDstFmt::U16 || dst_t == IDstFmt::S16;
    let mut b = ctx.fp_abs_neg_t(src, abs, neg, t);
    b = match round {
        RoundMode2::Round => ctx.fp_round_t(b, t),
        RoundMode2::Floor => ctx.fp_floor_t(b, t),
        RoundMode2::Ceil => ctx.fp_ceiling_t(b, t),
        RoundMode2::Trunc => ctx.fp_truncate_t(b, t),
        _ => b,
    };
    if !signed {
        // a negative float to uint cast is undefined, clamp first
        let c0 = if src_t == DstFmt::F64 {
            ctx.pack_double2x32_f(0.0)
        } else {
            ctx.ir.const_f(0.0)
        };
        b = ctx.fp_maximum_t(b, c0, t);
    }
    b = if src_t == DstFmt::F64 {
        if signed {
            ctx.fp64_convert_to_s32(b)
        } else {
            ctx.fp64_convert_to_u32(b)
        }
    } else if signed {
        ctx.fp32_convert_to_s32(b)
    } else {
        ctx.fp32_convert_to_u32(b)
    };
    if small {
        let min = get_int_min_d(dst_t)? as i32;
        let max = get_int_max_d(dst_t)? as i32;
        let lo = ctx.ir.const_(min);
        let hi = ctx.ir.const_(max);
        b = if signed {
            ctx.i_clamp_s32(b, lo, hi)
        } else {
            ctx.i_clamp_u32(b, lo, hi)
        };
    }
    let dest = get_dest(ctx, rd);
    ctx.copy_to(dest, b);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_i2f(
    ctx: &mut Ctx,
    src_t: ISrcFmt,
    dst_t: DstFmt,
    src: Opd,
    byte_sel: ByteSel,
    rd: i32,
    abs: bool,
    neg: bool,
) {
    let signed = matches!(
        src_t,
        ISrcFmt::S8 | ISrcFmt::S16 | ISrcFmt::S32 | ISrcFmt::S64
    );
    let small = matches!(
        src_t,
        ISrcFmt::U16 | ISrcFmt::S16 | ISrcFmt::U8 | ISrcFmt::S8
    );
    let mut b = ctx.i_abs_neg(src, abs, neg);
    if small {
        let size = if matches!(src_t, ISrcFmt::U16 | ISrcFmt::S16) {
            16
        } else {
            8
        };
        let off = ctx.ir.const_(byte_sel.0 * 8);
        let sz = ctx.ir.const_(size);
        b = if signed {
            ctx.bitfield_extract_s32(b, off, sz)
        } else {
            ctx.bitfield_extract_u32(b, off, sz)
        };
    }
    b = if dst_t == DstFmt::F64 {
        if signed {
            ctx.i_convert_s32_to_fp64(b)
        } else {
            ctx.i_convert_u32_to_fp64(b)
        }
    } else if signed {
        ctx.i_convert_s32_to_fp32(b)
    } else {
        ctx.i_convert_u32_to_fp32(b)
    };
    write_fp(ctx, dst_t, b, rd);
}

#[allow(clippy::too_many_arguments)]
fn emit_i2i(
    ctx: &mut Ctx,
    src_t: ISrcDstFmt,
    dst_t: ISrcDstFmt,
    src: Opd,
    byte_sel: ByteSel,
    rd: i32,
    abs: bool,
    neg: bool,
    sat: bool,
    wcc: bool,
) -> R {
    if (src_t.0 & !ISrcDstFmt::S8.0) > ISrcDstFmt::U32.0
        || (dst_t.0 & !ISrcDstFmt::S8.0) > ISrcDstFmt::U32.0
    {
        ctx.acc.log("Invalid I2I encoding.");
        return Ok(());
    }
    let src_signed = matches!(src_t, ISrcDstFmt::S8 | ISrcDstFmt::S16 | ISrcDstFmt::S32);
    let dst_signed = matches!(dst_t, ISrcDstFmt::S8 | ISrcDstFmt::S16 | ISrcDstFmt::S32);
    let src_small = matches!(
        src_t,
        ISrcDstFmt::U16 | ISrcDstFmt::S16 | ISrcDstFmt::U8 | ISrcDstFmt::S8
    );
    let mut s = src;
    if src_small {
        let size = if matches!(src_t, ISrcDstFmt::U16 | ISrcDstFmt::S16) {
            16
        } else {
            8
        };
        let off = ctx.ir.const_(byte_sel.0 * 8);
        let sz = ctx.ir.const_(size);
        s = if src_signed {
            ctx.bitfield_extract_s32(s, off, sz)
        } else {
            ctx.bitfield_extract_u32(s, off, sz)
        };
    }
    s = ctx.i_abs_neg(s, abs, neg);
    if sat {
        let min = get_int_min(dst_t)? as i32;
        let max = get_int_max(dst_t)? as i32;
        let lo = ctx.ir.const_(min);
        let hi = ctx.ir.const_(max);
        s = if dst_signed {
            ctx.i_clamp_s32(s, lo, hi)
        } else {
            ctx.i_clamp_u32(s, lo, hi)
        };
    }
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, s);
    set_zn_flags(ctx, s, wcc, false);
    Ok(())
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    macro_rules! f2f {
        ($ty:ident, $unpack:expr) => {{
            let op = $ty(raw);
            (|| {
                let src = $unpack(ctx, &op)?;
                emit_f2f(
                    ctx,
                    op.src_fmt(),
                    op.dst_fmt(),
                    op.round_mode(),
                    src,
                    op.dest(),
                    op.abs_b(),
                    op.neg_b(),
                    op.sat(),
                );
                Ok(())
            })()
        }};
    }
    macro_rules! f2i {
        ($ty:ident, $unpack:expr) => {{
            let op = $ty(raw);
            (|| {
                let src = $unpack(ctx, &op)?;
                emit_f2i(
                    ctx,
                    op.src_fmt(),
                    op.i_dst_fmt(),
                    op.round_mode(),
                    src,
                    op.dest(),
                    op.abs_b(),
                    op.neg_b(),
                )
            })()
        }};
    }
    macro_rules! i2f {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let src = src_b!($k, ctx, op);
            emit_i2f(
                ctx,
                op.i_src_fmt(),
                op.dst_fmt(),
                src,
                op.byte_sel(),
                op.dest(),
                op.abs_b(),
                op.neg_b(),
            );
            Ok(())
        }};
    }
    macro_rules! i2i {
        ($ty:ident, $k:ident) => {{
            let op = $ty(raw);
            let src = src_b!($k, ctx, op);
            emit_i2i(
                ctx,
                op.i_src_fmt(),
                op.i_dst_fmt(),
                src,
                op.byte_sel(),
                op.dest(),
                op.abs_b(),
                op.neg_b(),
                op.sat(),
                op.write_cc(),
            )
        }};
    }
    Some(match emit {
        Emit::F2fR => f2f!(InstF2fR, |c: &mut Ctx, o: &InstF2fR| unpack_reg(
            c,
            o.src_fmt(),
            o.sh(),
            o.src_b()
        )),
        Emit::F2fI => f2f!(InstF2fI, |c: &mut Ctx, o: &InstF2fI| unpack_imm(
            c,
            o.src_fmt(),
            o.sh(),
            imm20_to_float(o.imm20())
        )),
        Emit::F2fC => f2f!(InstF2fC, |c: &mut Ctx, o: &InstF2fC| unpack_cbuf(
            c,
            o.src_fmt(),
            o.sh(),
            o.cbuf_slot(),
            o.cbuf_offset()
        )),
        Emit::F2iR => f2i!(InstF2iR, |c: &mut Ctx, o: &InstF2iR| unpack_reg(
            c,
            o.src_fmt(),
            o.sh(),
            o.src_b()
        )),
        Emit::F2iI => f2i!(InstF2iI, |c: &mut Ctx, o: &InstF2iI| unpack_imm(
            c,
            o.src_fmt(),
            o.sh(),
            imm20_to_float(o.imm20())
        )),
        Emit::F2iC => f2i!(InstF2iC, |c: &mut Ctx, o: &InstF2iC| unpack_cbuf(
            c,
            o.src_fmt(),
            o.sh(),
            o.cbuf_slot(),
            o.cbuf_offset()
        )),
        Emit::I2fR => i2f!(InstI2fR, R),
        Emit::I2fI => i2f!(InstI2fI, S),
        Emit::I2fC => i2f!(InstI2fC, C),
        Emit::I2iR => i2i!(InstI2iR, R),
        Emit::I2iI => i2i!(InstI2iI, S),
        Emit::I2iC => i2i!(InstI2iC, C),
        _ => return None,
    })
}
