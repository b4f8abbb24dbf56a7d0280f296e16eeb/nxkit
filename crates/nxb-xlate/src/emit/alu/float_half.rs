//! Half precision arithmetic of `InstEmitFloatArithmetic.cs`.

use crate::emit::emitter::EmitterContext;
use crate::emit::helper::*;
use crate::ir::ops::Opd;
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

fn emit_hadd2_hmul2(
    ctx: &mut Ctx,
    fmt: OFmt,
    a: [Opd; 2],
    b: [Opd; 2],
    is_add: bool,
    rd: i32,
    sat: bool,
) -> R {
    let mut res = [a[0]; 2];
    for i in 0..2 {
        let r = if is_add {
            ctx.fp_add(a[i], b[i])
        } else {
            ctx.fp_multiply(a[i], b[i])
        };
        res[i] = ctx.fp_saturate_if(r, sat);
    }
    let d = get_dest(ctx, rd);
    let p = get_half_packed(ctx, fmt, res, rd)?;
    ctx.copy_to(d, p);
    Ok(())
}

fn emit_hfma2(
    ctx: &mut Ctx,
    fmt: OFmt,
    a: [Opd; 2],
    b: [Opd; 2],
    c: [Opd; 2],
    rd: i32,
    sat: bool,
) -> R {
    let mut res = [a[0]; 2];
    for i in 0..2 {
        let r = ctx.fp_fused_multiply_add(a[i], b[i], c[i]);
        res[i] = ctx.fp_saturate_if(r, sat);
    }
    let d = get_dest(ctx, rd);
    let p = get_half_packed(ctx, fmt, res, rd)?;
    ctx.copy_to(d, p);
    Ok(())
}

/// Half precision arithmetic (`Hadd2`, `Hfma2`, `Hmul2`).
pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    Some((|| -> R {
        match emit {
            Emit::Hadd2R => {
                let op = InstHadd2R(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_reg(ctx, op.b_swizzle(), op.src_b(), op.neg_b(), op.abs_b())?;
                emit_hadd2_hmul2(ctx, op.o_fmt(), a, b, true, op.dest(), op.sat())
            }
            Emit::Hadd2I => {
                let op = InstHadd2I(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_imm10(ctx, op.bimm_h0(), op.bimm_h1());
                emit_hadd2_hmul2(ctx, op.o_fmt(), a, b, true, op.dest(), op.sat())
            }
            Emit::Hadd2C => {
                let op = InstHadd2C(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_cbuf(
                    ctx,
                    HalfSwizzle::F32,
                    op.cbuf_slot(),
                    op.cbuf_offset(),
                    op.neg_b(),
                    op.abs_b(),
                )?;
                emit_hadd2_hmul2(ctx, op.o_fmt(), a, b, true, op.dest(), op.sat())
            }
            Emit::Hadd232i => {
                let op = InstHadd232i(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), false)?;
                let b = get_half_src_imm32(ctx, op.imm());
                emit_hadd2_hmul2(ctx, OFmt::F16, a, b, true, op.dest(), op.sat())
            }
            Emit::Hfma2R => {
                let op = InstHfma2R(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), false, false)?;
                let b = get_half_src_reg(ctx, op.b_swizzle(), op.src_b(), op.neg_a(), false)?;
                let c = get_half_src_reg(ctx, op.c_swizzle(), op.src_c(), op.neg_c(), false)?;
                emit_hfma2(ctx, op.o_fmt(), a, b, c, op.dest(), op.sat())
            }
            Emit::Hfma2I => {
                let op = InstHfma2I(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), false, false)?;
                let b = get_half_src_imm10(ctx, op.bimm_h0(), op.bimm_h1());
                let c = get_half_src_reg(ctx, op.c_swizzle(), op.src_c(), op.neg_c(), false)?;
                emit_hfma2(ctx, op.o_fmt(), a, b, c, op.dest(), op.sat())
            }
            Emit::Hfma2C => {
                let op = InstHfma2C(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), false, false)?;
                let b = get_half_src_cbuf(
                    ctx,
                    HalfSwizzle::F32,
                    op.cbuf_slot(),
                    op.cbuf_offset(),
                    op.neg_a(),
                    false,
                )?;
                let c = get_half_src_reg(ctx, op.c_swizzle(), op.src_c(), op.neg_c(), false)?;
                emit_hfma2(ctx, op.o_fmt(), a, b, c, op.dest(), op.sat())
            }
            Emit::Hfma2Rc => {
                let op = InstHfma2Rc(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), false, false)?;
                let b = get_half_src_reg(ctx, op.c_swizzle(), op.src_c(), op.neg_a(), false)?;
                let c = get_half_src_cbuf(
                    ctx,
                    HalfSwizzle::F32,
                    op.cbuf_slot(),
                    op.cbuf_offset(),
                    op.neg_c(),
                    false,
                )?;
                emit_hfma2(ctx, op.o_fmt(), a, b, c, op.dest(), op.sat())
            }
            Emit::Hfma232i => {
                let op = InstHfma232i(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), false, false)?;
                let b = get_half_src_imm32(ctx, op.imm());
                let c = get_half_src_reg(ctx, HalfSwizzle::F16, op.dest(), op.neg_c(), false)?;
                emit_hfma2(ctx, OFmt::F16, a, b, c, op.dest(), false)
            }
            Emit::Hmul2R => {
                let op = InstHmul2R(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), false, op.abs_a())?;
                let b = get_half_src_reg(ctx, op.b_swizzle(), op.src_b(), op.neg_a(), op.abs_b())?;
                emit_hadd2_hmul2(ctx, op.o_fmt(), a, b, false, op.dest(), op.sat())
            }
            Emit::Hmul2I => {
                let op = InstHmul2I(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), op.neg_a(), op.abs_a())?;
                let b = get_half_src_imm10(ctx, op.bimm_h0(), op.bimm_h1());
                emit_hadd2_hmul2(ctx, op.o_fmt(), a, b, false, op.dest(), op.sat())
            }
            Emit::Hmul2C => {
                let op = InstHmul2C(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), false, op.abs_a())?;
                let b = get_half_src_cbuf(
                    ctx,
                    HalfSwizzle::F32,
                    op.cbuf_slot(),
                    op.cbuf_offset(),
                    op.neg_a(),
                    op.abs_b(),
                )?;
                emit_hadd2_hmul2(ctx, op.o_fmt(), a, b, false, op.dest(), op.sat())
            }
            Emit::Hmul232i => {
                let op = InstHmul232i(raw);
                let a = get_half_src_reg(ctx, op.a_swizzle(), op.src_a(), false, false)?;
                let b = get_half_src_imm32(ctx, op.imm32());
                emit_hadd2_hmul2(ctx, OFmt::F16, a, b, false, op.dest(), op.sat())
            }
            _ => Err(String::new()),
        }
    })())
    .and_then(|r| match r {
        Err(e) if e.is_empty() => None,
        o => Some(o),
    })
}
