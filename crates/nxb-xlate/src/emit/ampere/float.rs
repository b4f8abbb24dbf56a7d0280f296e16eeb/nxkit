//! Float instructions of `InstEmitAmpereAlu.cs`: FADD, FMUL, FFMA, FMNMX, FSET, FSETP, FSEL, MUFU, conversions.

use super::half::half_lanes;
use super::operand::*;
use crate::emit::alu::conv::{emit_f2f, emit_f2i, emit_i2f};
use crate::emit::alu::helper::{flush_denormal_pair, get_fp_comparison, get_pred_logical_op};
use crate::ir::Opd;
use crate::ir::emitter::{FP32, FP64};
use crate::ir::ops::regtype;
use crate::maxwell::ampere::AmpOp;
use crate::maxwell::enums::*;

fn c(ctx: &mut Ctx, v: i32) -> Opd {
    ctx.ir.const_(v)
}

fn cf(ctx: &mut Ctx, v: f32) -> Opd {
    ctx.ir.const_f(v)
}

pub(super) fn fadd(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let (a, b) = second(ctx, &x, false, true, true)?;
    let fa = float(ctx, &a)?;
    let fb = float(ctx, &b)?;
    let res = ctx.fp_add(fa, fb);
    let res = ctx.fp_saturate_if(res, x.bit(77));
    set(ctx, &x, res, false);
    Ok(())
}

/// `.FMZ`: a product with a zero factor is zero.
fn fmz(ctx: &mut Ctx, x: &AmpOp, a: Opd, b: Opd, res: Opd, zero_product: Opd) -> Opd {
    if !x.bit(76) {
        return res;
    }
    let z = cf(ctx, 0.0);
    let ea = ctx.fp_compare_equal(a, z);
    let z2 = cf(ctx, 0.0);
    let eb = ctx.fp_compare_equal(b, z2);
    let zero = ctx.bitwise_or(ea, eb);
    ctx.conditional_select(zero, zero_product, res)
}

pub(super) fn fmul(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let (a, b) = second(ctx, &x, false, true, true)?;
    let fa = float(ctx, &a)?;
    let fb = float(ctx, &b)?;
    let prod = ctx.fp_multiply(fa, fb);
    let z = cf(ctx, 0.0);
    let mut res = fmz(ctx, &x, fa, fb, prod, z);
    let scale = x.bits(84, 87);
    if scale == 0 {
        return Err(fail(ctx, "FMUL scale 0"));
    }
    if scale != 4 {
        let f = if scale > 4 {
            (1 << (scale - 4)) as f32
        } else {
            1.0 / (1 << (4 - scale)) as f32
        };
        let k = cf(ctx, f);
        res = ctx.fp_multiply(res, k);
    }
    let res = ctx.fp_saturate_if(res, x.bit(77));
    set(ctx, &x, res, false);
    Ok(())
}

pub(super) fn ffma(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let (a, b, cc) = alu_srcs(ctx, &x, false, true, true)?;
    let fa = float(ctx, &a)?;
    let fb = float(ctx, &b)?;
    let fc = float(ctx, &cc)?;
    let fma = ctx.fp_fused_multiply_add(fa, fb, fc);
    let res = fmz(ctx, &x, fa, fb, fma, fc);
    let res = ctx.fp_saturate_if(res, x.bit(77));
    set(ctx, &x, res, false);
    Ok(())
}

pub(super) fn fmnmx(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let (a, b) = second(ctx, &x, false, true, true)?;
    let sa = float(ctx, &a)?;
    let sb = float(ctx, &b)?;
    let p = pred(ctx, &x, 87, 90, false);
    let mn = ctx.fp_minimum(sa, sb);
    let mx = ctx.fp_maximum(sa, sb);
    let res = ctx.conditional_select(p, mn, mx);
    set(ctx, &x, res, false);
    Ok(())
}

/// `AFtz`: bit 80 of FSET and FSETP is `.FTZ`. Both operands are flushed, except one that a constant shields.
fn a_ftz(ctx: &mut Ctx, x: &AmpOp, a: Opd, b: Opd) -> (Opd, Opd) {
    if x.bit(80) {
        flush_denormal_pair(ctx, a, b)
    } else {
        (a, b)
    }
}

pub(super) fn fset(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let (a, b) = second(ctx, &x, false, true, true)?;
    let fa = float(ctx, &a)?;
    let fb = float(ctx, &b)?;
    let (fa, fb) = a_ftz(ctx, &x, fa, fb);
    let res = get_fp_comparison(ctx, FComp(x.bits(76, 80)), fa, fb, FP32)?;
    let p = pred(ctx, &x, 87, 90, false);
    let res = get_pred_logical_op(ctx, BoolOp(x.bits(74, 76)), res, p);
    let one = cf(ctx, 1.0);
    let zero = c(ctx, 0);
    let v = ctx.conditional_select(res, one, zero);
    set(ctx, &x, v, false);
    Ok(())
}

pub(super) fn fsetp(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let (a, b) = second(ctx, &x, false, true, true)?;
    let fa = float(ctx, &a)?;
    let fb = float(ctx, &b)?;
    let (fa, fb) = a_ftz(ctx, &x, fa, fb);
    let cmp = get_fp_comparison(ctx, FComp(x.bits(76, 80)), fa, fb, FP32)?;
    let p = pred(ctx, &x, 87, 90, false);
    let bop = BoolOp(x.bits(74, 76));
    let v0 = get_pred_logical_op(ctx, bop, cmp, p);
    set_pred(ctx, x.bits(81, 84), v0, false);
    let n = ctx.bitwise_not(cmp);
    let v1 = get_pred_logical_op(ctx, bop, n, p);
    set_pred(ctx, x.bits(84, 87), v1, false);
    Ok(())
}

pub(super) fn fsel(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let (a, b) = second(ctx, &x, false, true, true)?;
    let p = pred(ctx, &x, 87, 90, false);
    let fa = float(ctx, &a)?;
    let fb = float(ctx, &b)?;
    let v = ctx.conditional_select(p, fa, fb);
    set(ctx, &x, v, false);
    Ok(())
}

/// `ATurnsToRadians`: MUFU.SIN and MUFU.COS use only the fraction of their turns. t * 2pi in float rounds that fraction
/// away (0.4 rad steps at 2^20 turns), and Metal's fast sin and cos return 0 from about 2^20 turns on. t - rint(t) is
/// exact for every float and lies in [-0.5, 0.5], where the fast functions are accurate. A small t stays itself.
fn turns_to_radians(ctx: &mut Ctx, turns: Opd) -> Opd {
    let r = ctx.fp_round(turns);
    let f = ctx.fp_subtract(turns, r);
    let k = cf(ctx, 2.0 * std::f32::consts::PI);
    ctx.fp_multiply(f, k)
}

pub(super) fn mufu(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let half = x.bit(73);
    let s = one(ctx, &x, false, true, true)?;
    let mut res = if half {
        // MUFU.F16's bits 60..61 pick one half (0 .H0, 1 .H1), not a pair's swizzle where 1 is .F32
        let sel = if x.bits(60, 62) & 1 != 0 { 3 } else { 2 };
        half_lanes(ctx, &s, sel)?[0]
    } else {
        float(ctx, &s)?
    };
    let op = x.bits(74, 80);
    if half && (op == 6 || op == 7) {
        return Err(fail(ctx, "MUFU.F16 RCP64H / RSQ64H"));
    }
    res = match op {
        // Volta and later take the argument in turns. Maxwell's came through RRO in radians.
        0 => {
            let m = turns_to_radians(ctx, res);
            ctx.fp_cosine(m)
        }
        1 => {
            let m = turns_to_radians(ctx, res);
            ctx.fp_sine(m)
        }
        2 => ctx.fp_exponent_b2(res),
        3 => ctx.fp_logarithm_b2(res),
        4 => ctx.fp_reciprocal(res),
        5 => ctx.fp_reciprocal_square_root(res),
        6 | 7 => {
            let z = c(ctx, 0);
            let p = ctx.pack_double2x32(z, res);
            let r = if op == 6 {
                ctx.fp_reciprocal_t(p, FP64)
            } else {
                ctx.fp_reciprocal_square_root_t(p, FP64)
            };
            ctx.unpack_double2x32_high(r)
        }
        8 => ctx.fp_square_root(res),
        n => return Err(fail(ctx, &format!("MUFU op {n}"))),
    };
    let v = if half {
        let z = cf(ctx, 0.0);
        ctx.pack_half2x16(res, z)
    } else {
        res
    };
    set(ctx, &x, v, false);
    Ok(())
}

pub(super) fn f2i(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let s = one(ctx, &x, false, true, true)?;
    let src_size = x.bits(84, 86);
    if !matches!(src_size, 1 | 2) {
        return Err(fail(
            ctx,
            "F2I from a float that is neither 16-bit nor 32-bit",
        ));
    }
    let size = x.bits(75, 77);
    let signed = x.bit(72);
    if size > 2 {
        return Err(fail(ctx, &format!("F2I to a {}-bit integer", 8 << size)));
    }
    let dst = IDstFmt((if signed { 4 } else { 0 }) | size.max(1));
    let mut src = get(ctx, &s)?;
    if src_size == 1 {
        src = if matches!(x.bits(60, 62), 1 | 3) {
            ctx.unpack_half2x16_high(src)
        } else {
            ctx.unpack_half2x16_low(src)
        };
    }
    emit_f2i(
        ctx,
        DstFmt::F32,
        dst,
        RoundMode2(x.bits(78, 80)),
        src,
        x.rd(),
        s.abs,
        s.neg,
    )?;
    if size == 0 && x.rd() != RZ {
        let r = gpr(ctx, x.rd());
        let v = if signed {
            let lo = c(ctx, -128);
            let hi = c(ctx, 127);
            ctx.i_clamp_s32(r, lo, hi)
        } else {
            let lo = c(ctx, 0);
            let hi = c(ctx, 255);
            ctx.i_clamp_u32(r, lo, hi)
        };
        ctx.copy_to(r, v);
    }
    Ok(())
}

pub(super) fn i2f(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let src_size = x.bits(84, 86);
    let dst_size = x.bits(75, 77);
    if src_size > 2 || !matches!(dst_size, 1 | 2) {
        return Err(fail(ctx, "I2F size"));
    }
    let select = x.bits(60, 62);
    if select != 0 && src_size == 2 {
        return Err(fail(ctx, "I2F byte select of a 32-bit source"));
    }
    let src = ISrcFmt((if x.bit(74) { 4 } else { 0 }) | src_size);
    let s = one(ctx, &x, false, false, false)?;
    let v = get(ctx, &s)?;
    let rnd = x.bits(78, 80);
    // A 32-bit integer to a float with .RM / .RP / .RZ (78..79). The compiler's 32-bit division starts with I2F.U32.RP
    // so that its reciprocal estimate stays on the side its correction step expects.
    if src_size == 2 && dst_size == 2 && rnd != 0 {
        let signed = x.bit(74);
        let f = if signed {
            ctx.i_convert_s32_to_fp32(v)
        } else {
            ctx.i_convert_u32_to_fp32(v)
        };
        // f is an integer value. It is exact back in 32 bits unless it rounded up to 2^31 (signed) or 2^32, where it
        // is above v anyway.
        let limit = cf(ctx, if signed { 2147483648.0 } else { 4294967296.0 });
        let below = ctx.fp_compare_less(f, limit);
        let big = ctx.bitwise_not(below);
        let back = if signed {
            ctx.fp32_convert_to_s32(f)
        } else {
            ctx.fp32_convert_to_u32(f)
        };
        let above = if signed {
            ctx.i_compare_greater(back, v)
        } else {
            ctx.i_compare_greater_unsigned(back, v)
        };
        let gt = ctx.bitwise_or(big, above);
        let under = if signed {
            ctx.i_compare_less(back, v)
        } else {
            ctx.i_compare_less_unsigned(back, v)
        };
        let lt = ctx.bitwise_and(below, under);
        let neg = if signed {
            let z = c(ctx, 0);
            Some(ctx.i_compare_less(v, z))
        } else {
            None
        };
        let r = directed(ctx, rnd, f, gt, lt, neg);
        set(ctx, &x, r, false);
        return Ok(());
    }
    let sel = ByteSel(select * if src_size == 1 { 2 } else { 1 });
    emit_i2f(ctx, src, DstFmt(dst_size), v, sel, x.rd(), false, false);
    Ok(())
}

/// `ADirected`: a result r rounded to nearest, moved to the directed rounding .RM (1), .RP (2) or .RZ (3) asks for.
/// `gt` is r above the exact value, `lt` r below it, `neg` the value negative (`None`: never). One step of a float's
/// bits moves it to its neighbour of the same sign: + 1 away from zero, - 1 toward it. An overflow rounded to infinity
/// steps back to the largest finite value.
fn directed(ctx: &mut Ctx, rnd: i32, r: Opd, gt: Opd, lt: Opd, neg: Option<Opd>) -> Opd {
    let pos = neg.map(|n| ctx.bitwise_not(n));
    let mut and = |a: Option<Opd>, b: Option<Opd>| match (a, b) {
        (Some(a), Some(b)) => Some(ctx.bitwise_and(a, b)),
        _ => None,
    };
    let (away, toward) = match rnd {
        // RP: below the value, step up
        2 => (
            if neg.is_none() {
                Some(lt)
            } else {
                and(Some(lt), pos)
            },
            and(Some(lt), neg),
        ),
        // RM: above the value, step down
        1 => (
            and(Some(gt), neg),
            if neg.is_none() {
                Some(gt)
            } else {
                and(Some(gt), pos)
            },
        ),
        // RZ: larger in magnitude, step toward zero
        _ => {
            let t = match (and(Some(gt), pos), and(Some(lt), neg)) {
                (Some(a), Some(b)) => ctx.bitwise_or(a, b),
                _ => gt,
            };
            (None, Some(t))
        }
    };
    let mut r = r;
    if let Some(a) = away {
        let one = c(ctx, 1);
        let up = ctx.i_add(r, one);
        r = ctx.conditional_select(a, up, r);
    }
    if let Some(t) = toward {
        let one = c(ctx, -1);
        let down = ctx.i_add(r, one);
        r = ctx.conditional_select(t, down, r);
    }
    r
}

pub(super) fn f2i64(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let s = one(ctx, &x, false, true, true)?;
    if x.bits(84, 86) != 2 || x.bits(75, 77) != 3 || x.bit(72) || x.bits(78, 80) != 3 {
        return Err(fail(
            ctx,
            "F2I to 64 bits other than an unsigned truncation of an F32",
        ));
    }
    let f = float(ctx, &s)?;
    let k = cf(ctx, 1.0 / 4294967296.0);
    let m = ctx.fp_multiply(f, k);
    let hi = ctx.fp32_convert_to_u32(m);
    let h = ctx.i_convert_u32_to_fp32(hi);
    let k2 = cf(ctx, 4294967296.0);
    let hm = ctx.fp_multiply(h, k2);
    let sub = ctx.fp_subtract(f, hm);
    let lo = ctx.fp32_convert_to_u32(sub);
    if x.rd() < RZ {
        let d = gpr(ctx, x.rd());
        ctx.copy_to(d, lo);
    }
    if x.rd() + 1 < RZ {
        let d = gpr(ctx, x.rd() + 1);
        ctx.copy_to(d, hi);
    }
    Ok(())
}

pub(super) fn i2f64(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    if x.bits(84, 86) != 3 || x.bits(75, 77) != 2 || x.opcode() != 0x312 {
        return Err(fail(
            ctx,
            "I2F from 64 bits other than a register pair to an F32",
        ));
    }
    let lo = if x.rb() >= RZ {
        c(ctx, 0)
    } else {
        gpr(ctx, x.rb())
    };
    let hi = if x.rb() + 1 >= RZ {
        c(ctx, 0)
    } else {
        gpr(ctx, x.rb() + 1)
    };
    let fh = if x.bit(74) {
        ctx.i_convert_s32_to_fp32(hi)
    } else {
        ctx.i_convert_u32_to_fp32(hi)
    };
    let k = cf(ctx, 4294967296.0);
    let m = ctx.fp_multiply(fh, k);
    let fl = ctx.i_convert_u32_to_fp32(lo);
    let v = ctx.fp_add(m, fl);
    set(ctx, &x, v, false);
    Ok(())
}

pub(super) fn i2fp(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let s = one(ctx, &x, false, false, false)?;
    let src = get(ctx, &s)?;
    let v = if x.bit(74) {
        ctx.i_convert_s32_to_fp32(src)
    } else {
        ctx.i_convert_u32_to_fp32(src)
    };
    set(ctx, &x, v, false);
    Ok(())
}

pub(super) fn frnd(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    if x.bits(84, 86) != 2 || x.bits(75, 77) != 2 {
        return Err(fail(ctx, "FRND of a float that is not 32-bit"));
    }
    let s = one(ctx, &x, false, true, true)?;
    let src = float(ctx, &s)?;
    let res = match x.bits(78, 80) {
        0 => ctx.fp_round(src),
        1 => ctx.fp_floor(src),
        2 => ctx.fp_ceiling(src),
        _ => ctx.fp_truncate(src),
    };
    set(ctx, &x, res, false);
    Ok(())
}

pub(super) fn f2f(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let s = one(ctx, &x, false, true, true)?;
    let mut src_size = x.bits(84, 86);
    let dst_size = x.bits(75, 77);
    if !(1..=3).contains(&src_size) || !(1..=3).contains(&dst_size) {
        return Err(fail(ctx, "F2F size"));
    }
    let mut src = get(ctx, &s)?;
    if src_size == 1 {
        src = if matches!(x.bits(60, 62), 1 | 3) {
            ctx.unpack_half2x16_high(src)
        } else {
            ctx.unpack_half2x16_low(src)
        };
        src_size = 2;
    } else if src_size == 3 {
        let hi = get_high(ctx, &s);
        src = ctx.pack_double2x32(src, hi);
    }
    emit_f2f(
        ctx,
        DstFmt(src_size),
        DstFmt(dst_size),
        IntegerRound::Pass,
        src,
        x.rd(),
        s.abs,
        s.neg,
        false,
    );
    // F2F.F16.F32 .RM / .RP / .RZ (78..79): the pack rounds to nearest. The half read back (exact in 32 bits) tells
    // which side of the float it fell. The read goes through a mask, because the optimizer takes an unpack of a pack
    // for the packed float itself, which would skip the rounding.
    let rnd = x.bits(78, 80);
    if src_size == 2 && dst_size == 1 && rnd != 0 && x.rd() != RZ {
        let f = ctx.fp_abs_neg(src, s.abs, s.neg);
        let rd = gpr(ctx, x.rd());
        let mask = c(ctx, 0xffff);
        let low = ctx.bitwise_and(rd, mask);
        let back = ctx.unpack_half2x16_low(low);
        let gt = ctx.fp_compare_less(f, back);
        let lt = ctx.fp_compare_less(back, f);
        let z = cf(ctx, 0.0);
        let neg = ctx.fp_compare_less(f, z);
        let res = directed(ctx, rnd, rd, gt, lt, Some(neg));
        let d = gpr(ctx, x.rd());
        ctx.copy_to(d, res);
    }
    Ok(())
}

pub(super) fn f2fp(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    if x.bit(72) {
        return Err(fail(ctx, "F2FP bit 72"));
    }
    let (a, b) = second(ctx, &x, false, true, false)?;
    let u = is_uniform(ctx);
    if x.bit(78) {
        let fb = float(ctx, &b)?;
        let z = cf(ctx, 0.0);
        let low = ctx.pack_half2x16(fb, z);
        let rc = gpr(ctx, x.rc());
        let k = c(ctx, 16);
        let high = ctx.shift_left(rc, k);
        let v = ctx.bitwise_or(high, low);
        set(ctx, &x, v, u);
        return Ok(());
    }
    let fb = float(ctx, &b)?;
    let fa = float(ctx, &a)?;
    let v = ctx.pack_half2x16(fb, fa);
    set(ctx, &x, v, u);
    Ok(())
}

/// F2IP.U8 Rd, a, b: a and b as saturated bytes, a in byte 1 and b in byte 0.
pub(super) fn f2ip(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    if x.bits(72, 78) != 4 || x.rc() != RZ {
        return Err(fail(ctx, "F2IP form"));
    }
    let (a, b) = second(ctx, &x, false, true, false)?;
    let mode = RoundMode2(x.bits(78, 80));
    let u = is_uniform(ctx);
    let byte = |ctx: &mut Ctx, f: Opd| {
        let f = match mode {
            RoundMode2::Round => ctx.fp_round(f),
            RoundMode2::Floor => ctx.fp_floor(f),
            RoundMode2::Ceil => ctx.fp_ceiling(f),
            _ => ctx.fp_truncate(f),
        };
        let zero = cf(ctx, 0.0);
        let top = cf(ctx, 255.0);
        let lo = ctx.fp_maximum(f, zero);
        let clamped = ctx.fp_minimum(lo, top);
        ctx.fp32_convert_to_u32(clamped)
    };
    let fa = float(ctx, &a)?;
    let fb = float(ctx, &b)?;
    let high = byte(ctx, fa);
    let low = byte(ctx, fb);
    let k = c(ctx, 8);
    let shifted = ctx.shift_left(high, k);
    let v = ctx.bitwise_or(shifted, low);
    set(ctx, &x, v, u);
    Ok(())
}

pub(super) fn fswzadd(ctx: &mut Ctx) -> R {
    let x = x(ctx);
    let a = gpr(ctx, x.ra());
    let b = ctx.ir.register(x.rc(), regtype::GPR);
    let v = ctx.fp_swizzle_add(a, b, x.bits(32, 40));
    set(ctx, &x, v, false);
    Ok(())
}
