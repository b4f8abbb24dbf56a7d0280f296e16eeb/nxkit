//! `InstEmitFloatArithmetic.cs`, `InstEmitMultifunction.cs`.

use super::helper::set_fp_zn_flags;
use crate::emit::emitter::{EmitterContext, FP32, FP64};
use crate::emit::helper::*;
use crate::ir::inst::Inst;
use crate::ir::ops::Opd;
use crate::maxwell::enums::*;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

#[allow(clippy::too_many_arguments)]
fn emit_fadd(
    ctx: &mut Ctx,
    fp: Inst,
    a: Opd,
    b: Opd,
    rd: i32,
    na: bool,
    nb: bool,
    aa: bool,
    ab: bool,
    sat: bool,
    wcc: bool,
) {
    let is64 = fp == FP64;
    let a = ctx.fp_abs_neg_t(a, aa, na, fp);
    let b = ctx.fp_abs_neg_t(b, ab, nb, fp);
    let s = ctx.fp_add_t(a, b, fp);
    let res = ctx.fp_saturate_if_t(s, sat, fp);
    set_dest(ctx, res, rd, is64);
    set_fp_zn_flags(ctx, res, wcc, fp);
}

#[allow(clippy::too_many_arguments)]
fn emit_ffma(
    ctx: &mut Ctx,
    fp: Inst,
    a: Opd,
    b: Opd,
    c: Opd,
    rd: i32,
    nb: bool,
    nc: bool,
    sat: bool,
    wcc: bool,
) {
    let is64 = fp == FP64;
    let b = ctx.fp_negate_if_t(b, nb, fp);
    let c = ctx.fp_negate_if_t(c, nc, fp);
    let m = ctx.fp_fused_multiply_add_t(a, b, c, fp);
    let res = ctx.fp_saturate_if_t(m, sat, fp);
    set_dest(ctx, res, rd, is64);
    set_fp_zn_flags(ctx, res, wcc, fp);
}

#[allow(clippy::too_many_arguments)]
fn emit_fmul(
    ctx: &mut Ctx,
    fp: Inst,
    scale: MultiplyScale,
    a: Opd,
    b: Opd,
    rd: i32,
    nb: bool,
    sat: bool,
    wcc: bool,
) {
    let is64 = fp == FP64;
    let b = ctx.fp_negate_if_t(b, nb, fp);
    let mut a = a;
    if scale != MultiplyScale::NoScale {
        let v = match scale {
            MultiplyScale::D2 => 0.5,
            MultiplyScale::D4 => 0.25,
            MultiplyScale::D8 => 0.125,
            MultiplyScale::M2 => 2.0,
            MultiplyScale::M4 => 4.0,
            MultiplyScale::M8 => 8.0,
            _ => 1.0,
        };
        let mut sc = ctx.ir.const_f(v);
        if v == 1.0 {
            ctx.acc.log(&format!(
                "Invalid FP multiply scale \"{}\".",
                scale_name(scale)
            ));
        }
        if is64 {
            sc = ctx.fp32_convert_to_fp64(sc);
        }
        a = ctx.fp_multiply_t(a, sc, fp);
    }
    let m = ctx.fp_multiply_t(a, b, fp);
    let res = ctx.fp_saturate_if_t(m, sat, fp);
    set_dest(ctx, res, rd, is64);
    set_fp_zn_flags(ctx, res, wcc, fp);
}

fn scale_name(s: MultiplyScale) -> String {
    match s {
        MultiplyScale::NoScale => "NoScale".into(),
        MultiplyScale::D2 => "D2".into(),
        MultiplyScale::D4 => "D4".into(),
        MultiplyScale::D8 => "D8".into(),
        MultiplyScale::M8 => "M8".into(),
        MultiplyScale::M4 => "M4".into(),
        MultiplyScale::M2 => "M2".into(),
        _ => s.0.to_string(),
    }
}

fn emit_rro(ctx: &mut Ctx, b: Opd, rd: i32, abs: bool, neg: bool) {
    // range reduction operator: a plain move, always followed by a matching MUFU
    let b = ctx.fp_abs_neg(b, abs, neg);
    let d = get_dest(ctx, rd);
    ctx.copy_to(d, b);
}

/// Multifunction `Mufu`.
fn mufu(ctx: &mut Ctx) {
    let op = InstMufu(ctx.curr_op.raw);
    let s = get_src_reg(ctx, op.src_a(), false);
    let mut res = ctx.fp_abs_neg(s, op.abs_a(), op.neg_a());
    match op.mufu_op() {
        MufuOp::Cos => res = ctx.fp_cosine(res),
        MufuOp::Sin => res = ctx.fp_sine(res),
        MufuOp::Ex2 => res = ctx.fp_exponent_b2(res),
        MufuOp::Lg2 => res = ctx.fp_logarithm_b2(res),
        MufuOp::Rcp => res = ctx.fp_reciprocal(res),
        MufuOp::Rsq => res = ctx.fp_reciprocal_square_root(res),
        MufuOp::Rcp64h => {
            let z = ctx.ir.const_(0);
            res = ctx.pack_double2x32(z, res);
            let r = ctx.fp_reciprocal_t(res, FP64);
            res = ctx.unpack_double2x32_high(r);
        }
        MufuOp::Rsq64h => {
            let z = ctx.ir.const_(0);
            res = ctx.pack_double2x32(z, res);
            let r = ctx.fp_reciprocal_square_root_t(res, FP64);
            res = ctx.unpack_double2x32_high(r);
        }
        MufuOp::Sqrt => res = ctx.fp_square_root(res),
        o => ctx.acc.log(&format!("Invalid MUFU operation \"{}\".", o.0)),
    }
    let d = get_dest(ctx, op.dest());
    let s = ctx.fp_saturate_if(res, op.sat());
    ctx.copy_to(d, s);
}

fn imm20(i: i32) -> i32 {
    imm20_to_float(i)
}

/// Float arithmetic emitters.
pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    let raw = ctx.curr_op.raw;
    macro_rules! dadd {
        ($ty:ident, $b:expr) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), true);
            let b = $b(ctx, &op);
            emit_fadd(
                ctx,
                FP64,
                a,
                b,
                op.dest(),
                op.neg_a(),
                op.neg_b(),
                op.abs_a(),
                op.abs_b(),
                false,
                op.write_cc(),
            );
        }};
    }
    macro_rules! fadd {
        ($ty:ident, $b:expr, $sat:expr) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), false);
            let b = $b(ctx, &op);
            let sat = $sat(&op);
            emit_fadd(
                ctx,
                FP32,
                a,
                b,
                op.dest(),
                op.neg_a(),
                op.neg_b(),
                op.abs_a(),
                op.abs_b(),
                sat,
                op.write_cc(),
            );
        }};
    }
    macro_rules! ffma {
        ($ty:ident, $fp:expr, $b:expr, $c:expr, $sat:expr) => {{
            let op = $ty(raw);
            let is64 = $fp == FP64;
            let a = get_src_reg(ctx, op.src_a(), is64);
            let b = $b(ctx, &op);
            let c = $c(ctx, &op);
            let sat = $sat(&op);
            emit_ffma(
                ctx,
                $fp,
                a,
                b,
                c,
                op.dest(),
                op.neg_a(),
                op.neg_c(),
                sat,
                op.write_cc(),
            );
        }};
    }
    macro_rules! fmul {
        ($ty:ident, $fp:expr, $scale:expr, $b:expr, $neg:expr, $sat:expr) => {{
            let op = $ty(raw);
            let a = get_src_reg(ctx, op.src_a(), $fp == FP64);
            let b = $b(ctx, &op);
            let scale = $scale(&op);
            let neg = $neg(&op);
            let sat = $sat(&op);
            emit_fmul(ctx, $fp, scale, a, b, op.dest(), neg, sat, op.write_cc());
        }};
    }
    let r: R = match emit {
        Emit::DaddR => {
            dadd!(InstDaddR, |c: &mut Ctx, o: &InstDaddR| get_src_reg(
                c,
                o.src_b(),
                true
            ));
            Ok(())
        }
        Emit::DaddI => {
            dadd!(InstDaddI, |c: &mut Ctx, o: &InstDaddI| get_src_imm(
                c,
                imm20(o.imm20()),
                true
            ));
            Ok(())
        }
        Emit::DaddC => {
            dadd!(InstDaddC, |c: &mut Ctx, o: &InstDaddC| get_src_cbuf(
                c,
                o.cbuf_slot(),
                o.cbuf_offset(),
                true
            ));
            Ok(())
        }
        Emit::FaddR => {
            fadd!(
                InstFaddR,
                |c: &mut Ctx, o: &InstFaddR| get_src_reg(c, o.src_b(), false),
                |o: &InstFaddR| o.sat()
            );
            Ok(())
        }
        Emit::FaddI => {
            fadd!(
                InstFaddI,
                |c: &mut Ctx, o: &InstFaddI| get_src_imm(c, imm20(o.imm20()), false),
                |o: &InstFaddI| o.sat()
            );
            Ok(())
        }
        Emit::FaddC => {
            fadd!(
                InstFaddC,
                |c: &mut Ctx, o: &InstFaddC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), false),
                |o: &InstFaddC| o.sat()
            );
            Ok(())
        }
        Emit::Fadd32i => {
            fadd!(
                InstFadd32i,
                |c: &mut Ctx, o: &InstFadd32i| get_src_imm(c, o.imm32(), false),
                |_o: &InstFadd32i| false
            );
            Ok(())
        }
        Emit::DfmaR => {
            ffma!(
                InstDfmaR,
                FP64,
                |c: &mut Ctx, o: &InstDfmaR| get_src_reg(c, o.src_b(), true),
                |c: &mut Ctx, o: &InstDfmaR| get_src_reg(c, o.src_c(), true),
                |_o: &InstDfmaR| false
            );
            Ok(())
        }
        Emit::DfmaI => {
            ffma!(
                InstDfmaI,
                FP64,
                |c: &mut Ctx, o: &InstDfmaI| get_src_imm(c, imm20(o.imm20()), true),
                |c: &mut Ctx, o: &InstDfmaI| get_src_reg(c, o.src_c(), true),
                |_o: &InstDfmaI| false
            );
            Ok(())
        }
        Emit::DfmaC => {
            ffma!(
                InstDfmaC,
                FP64,
                |c: &mut Ctx, o: &InstDfmaC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), true),
                |c: &mut Ctx, o: &InstDfmaC| get_src_reg(c, o.src_c(), true),
                |_o: &InstDfmaC| false
            );
            Ok(())
        }
        Emit::DfmaRc => {
            ffma!(
                InstDfmaRc,
                FP64,
                |c: &mut Ctx, o: &InstDfmaRc| get_src_reg(c, o.src_c(), true),
                |c: &mut Ctx, o: &InstDfmaRc| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), true),
                |_o: &InstDfmaRc| false
            );
            Ok(())
        }
        Emit::FfmaR => {
            ffma!(
                InstFfmaR,
                FP32,
                |c: &mut Ctx, o: &InstFfmaR| get_src_reg(c, o.src_b(), false),
                |c: &mut Ctx, o: &InstFfmaR| get_src_reg(c, o.src_c(), false),
                |o: &InstFfmaR| o.sat()
            );
            Ok(())
        }
        Emit::FfmaI => {
            ffma!(
                InstFfmaI,
                FP32,
                |c: &mut Ctx, o: &InstFfmaI| get_src_imm(c, imm20(o.imm20()), false),
                |c: &mut Ctx, o: &InstFfmaI| get_src_reg(c, o.src_c(), false),
                |o: &InstFfmaI| o.sat()
            );
            Ok(())
        }
        Emit::FfmaC => {
            ffma!(
                InstFfmaC,
                FP32,
                |c: &mut Ctx, o: &InstFfmaC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), false),
                |c: &mut Ctx, o: &InstFfmaC| get_src_reg(c, o.src_c(), false),
                |o: &InstFfmaC| o.sat()
            );
            Ok(())
        }
        Emit::FfmaRc => {
            ffma!(
                InstFfmaRc,
                FP32,
                |c: &mut Ctx, o: &InstFfmaRc| get_src_reg(c, o.src_c(), false),
                |c: &mut Ctx, o: &InstFfmaRc| get_src_cbuf(
                    c,
                    o.cbuf_slot(),
                    o.cbuf_offset(),
                    false
                ),
                |o: &InstFfmaRc| o.sat()
            );
            Ok(())
        }
        Emit::Ffma32i => {
            ffma!(
                InstFfma32i,
                FP32,
                |c: &mut Ctx, o: &InstFfma32i| get_src_imm(c, o.imm32(), false),
                |c: &mut Ctx, o: &InstFfma32i| get_src_reg(c, o.dest(), false),
                |o: &InstFfma32i| o.sat()
            );
            Ok(())
        }
        Emit::DmulR => {
            fmul!(
                InstDmulR,
                FP64,
                |_o: &InstDmulR| MultiplyScale::NoScale,
                |c: &mut Ctx, o: &InstDmulR| get_src_reg(c, o.src_b(), true),
                |o: &InstDmulR| o.neg_a(),
                |_o: &InstDmulR| false
            );
            Ok(())
        }
        Emit::DmulI => {
            fmul!(
                InstDmulI,
                FP64,
                |_o: &InstDmulI| MultiplyScale::NoScale,
                |c: &mut Ctx, o: &InstDmulI| get_src_imm(c, imm20(o.imm20()), true),
                |o: &InstDmulI| o.neg_a(),
                |_o: &InstDmulI| false
            );
            Ok(())
        }
        Emit::DmulC => {
            fmul!(
                InstDmulC,
                FP64,
                |_o: &InstDmulC| MultiplyScale::NoScale,
                |c: &mut Ctx, o: &InstDmulC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), true),
                |o: &InstDmulC| o.neg_a(),
                |_o: &InstDmulC| false
            );
            Ok(())
        }
        Emit::FmulR => {
            fmul!(
                InstFmulR,
                FP32,
                |o: &InstFmulR| o.scale(),
                |c: &mut Ctx, o: &InstFmulR| get_src_reg(c, o.src_b(), false),
                |o: &InstFmulR| o.neg_a(),
                |o: &InstFmulR| o.sat()
            );
            Ok(())
        }
        Emit::FmulI => {
            fmul!(
                InstFmulI,
                FP32,
                |o: &InstFmulI| o.scale(),
                |c: &mut Ctx, o: &InstFmulI| get_src_imm(c, imm20(o.imm20()), false),
                |o: &InstFmulI| o.neg_a(),
                |o: &InstFmulI| o.sat()
            );
            Ok(())
        }
        Emit::FmulC => {
            fmul!(
                InstFmulC,
                FP32,
                |o: &InstFmulC| o.scale(),
                |c: &mut Ctx, o: &InstFmulC| get_src_cbuf(c, o.cbuf_slot(), o.cbuf_offset(), false),
                |o: &InstFmulC| o.neg_a(),
                |o: &InstFmulC| o.sat()
            );
            Ok(())
        }
        Emit::Fmul32i => {
            fmul!(
                InstFmul32i,
                FP32,
                |_o: &InstFmul32i| MultiplyScale::NoScale,
                |c: &mut Ctx, o: &InstFmul32i| get_src_imm(c, o.imm32(), false),
                |_o: &InstFmul32i| false,
                |o: &InstFmul32i| o.sat()
            );
            Ok(())
        }
        Emit::Mufu => {
            mufu(ctx);
            Ok(())
        }
        Emit::RroR => {
            let op = InstRroR(raw);
            let b = get_src_reg(ctx, op.src_b(), false);
            emit_rro(ctx, b, op.dest(), op.abs_b(), op.neg_b());
            Ok(())
        }
        Emit::RroI => {
            let op = InstRroI(raw);
            let b = get_src_imm(ctx, imm20(op.imm20()), false);
            emit_rro(ctx, b, op.dest(), op.abs_b(), op.neg_b());
            Ok(())
        }
        Emit::RroC => {
            let op = InstRroC(raw);
            let b = get_src_cbuf(ctx, op.cbuf_slot(), op.cbuf_offset(), false);
            emit_rro(ctx, b, op.dest(), op.abs_b(), op.neg_b());
            Ok(())
        }
        _ => return None,
    };
    Some(r)
}
