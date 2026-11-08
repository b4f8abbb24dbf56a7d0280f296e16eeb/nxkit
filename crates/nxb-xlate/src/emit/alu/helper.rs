//! `InstEmitAluHelper.cs`.

use crate::emit::helper::{get_nf, get_zf, sign_extend_to32, zero_extend_to32};
use crate::ir::emitter::{EmitterContext, FP32, FP64};
use crate::ir::inst::Inst;
use crate::ir::ops::Opd;
use crate::maxwell::enums::{BoolOp, FComp, IDstFmt, ISrcDstFmt, VectorSelect};

/// `FlushDenormal`: a denormal operand of a `.FTZ` comparison compares as zero. A constant is decided here.
pub fn flush_denormal(ctx: &mut EmitterContext<'_, '_>, v: Opd) -> Opd {
    if ctx.ir.ty(v) == crate::ir::ops::OperandType::Constant {
        return if ctx.ir.as_float(v).is_subnormal() {
            ctx.ir.const_f(0.0)
        } else {
            v
        };
    }
    let abs = ctx.fp_absolute(v);
    let min = ctx.ir.const_f(f32::MIN_POSITIVE);
    let less = ctx.fp_compare_less(abs, min);
    let zero = ctx.ir.const_f(0.0);
    ctx.conditional_select(less, zero, v)
}

/// True when a constant operand (already flushed) decides a comparison the same way for a denormal and for zero.
///
/// That holds for NaN, for an infinity and for any normal number: a denormal lies between zero and the constant's
/// magnitude, exactly like zero does, so the flush of the other operand cannot change the result.
fn shields_flush(ctx: &EmitterContext<'_, '_>, v: Opd) -> bool {
    if ctx.ir.ty(v) != crate::ir::ops::OperandType::Constant {
        return false;
    }
    let f = ctx.ir.as_float(v);
    f.is_nan() || f.is_infinite() || f.is_normal()
}

/// `FlushDenormal` on both operands of a `.FTZ` comparison.
///
/// The flush of one operand is left out when the other operand is a constant that shields it (`shields_flush`).
/// A comparison against a literal is the common case, and the flush costs three operations.
pub fn flush_denormal_pair(ctx: &mut EmitterContext<'_, '_>, a: Opd, b: Opd) -> (Opd, Opd) {
    let (sa, sb) = (shields_flush(ctx, a), shields_flush(ctx, b));
    let fa = if sb { a } else { flush_denormal(ctx, a) };
    let fb = if sa { b } else { flush_denormal(ctx, b) };
    (fa, fb)
}

/// `GetIntMin(IDstFmt)`.
pub fn get_int_min_d(t: IDstFmt) -> Result<i64, String> {
    Ok(match t {
        IDstFmt::U16 | IDstFmt::U32 => 0,
        IDstFmt::S16 => i16::MIN as i64,
        IDstFmt::S32 => i32::MIN as i64,
        _ => {
            return Err(format!(
                "The type \"{}\" is not a supported integer type.",
                t.0
            ));
        }
    })
}

/// `GetIntMax(IDstFmt)`.
pub fn get_int_max_d(t: IDstFmt) -> Result<i64, String> {
    Ok(match t {
        IDstFmt::U16 => u16::MAX as i64,
        IDstFmt::S16 => i16::MAX as i64,
        IDstFmt::U32 => u32::MAX as i64,
        IDstFmt::S32 => i32::MAX as i64,
        _ => {
            return Err(format!(
                "The type \"{}\" is not a supported integer type.",
                t.0
            ));
        }
    })
}

/// `GetIntMin(ISrcDstFmt)`.
pub fn get_int_min(t: ISrcDstFmt) -> Result<i64, String> {
    Ok(match t {
        ISrcDstFmt::U8 | ISrcDstFmt::U16 | ISrcDstFmt::U32 => 0,
        ISrcDstFmt::S8 => i8::MIN as i64,
        ISrcDstFmt::S16 => i16::MIN as i64,
        ISrcDstFmt::S32 => i32::MIN as i64,
        _ => {
            return Err(format!(
                "The type \"{}\" is not a supported integer type.",
                t.0
            ));
        }
    })
}

/// `GetIntMax(ISrcDstFmt)`.
pub fn get_int_max(t: ISrcDstFmt) -> Result<i64, String> {
    Ok(match t {
        ISrcDstFmt::U8 => u8::MAX as i64,
        ISrcDstFmt::S8 => i8::MAX as i64,
        ISrcDstFmt::U16 => u16::MAX as i64,
        ISrcDstFmt::S16 => i16::MAX as i64,
        ISrcDstFmt::U32 => u32::MAX as i64,
        ISrcDstFmt::S32 => i32::MAX as i64,
        _ => {
            return Err(format!(
                "The type \"{}\" is not a supported integer type.",
                t.0
            ));
        }
    })
}

/// `GetPredLogicalOp`.
pub fn get_pred_logical_op(
    ctx: &mut EmitterContext<'_, '_>,
    op: BoolOp,
    input: Opd,
    pred: Opd,
) -> Opd {
    match op {
        BoolOp::And => ctx.bitwise_and(input, pred),
        BoolOp::Or => ctx.bitwise_or(input, pred),
        BoolOp::Xor => ctx.bitwise_exclusive_or(input, pred),
        _ => input,
    }
}

/// `Extend`.
pub fn extend(ctx: &mut EmitterContext<'_, '_>, src: Opd, t: VectorSelect) -> Opd {
    let (shift, bits, signed) = match t {
        VectorSelect::U8B0 => (0, 8, false),
        VectorSelect::U8B1 => (8, 8, false),
        VectorSelect::U8B2 => (16, 8, false),
        VectorSelect::U8B3 => (24, 8, false),
        VectorSelect::U16H0 => (0, 16, false),
        VectorSelect::U16H1 => (16, 16, false),
        VectorSelect::S8B0 => (0, 8, true),
        VectorSelect::S8B1 => (8, 8, true),
        VectorSelect::S8B2 => (16, 8, true),
        VectorSelect::S8B3 => (24, 8, true),
        VectorSelect::S16H0 => (0, 16, true),
        VectorSelect::S16H1 => (16, 16, true),
        _ => return src,
    };
    let c = ctx.ir.const_(shift);
    let s = ctx.shift_right_u32(src, c);
    if signed {
        sign_extend_to32(ctx, s, bits)
    } else {
        zero_extend_to32(ctx, s, bits)
    }
}

/// `SetZnFlags`.
pub fn set_zn_flags(ctx: &mut EmitterContext<'_, '_>, dest: Opd, set_cc: bool, extended: bool) {
    if !set_cc {
        return;
    }
    let zero = ctx.ir.const_(0);
    if extended {
        let old_zf = get_zf(ctx);
        let eq = ctx.i_compare_equal(dest, zero);
        let res = ctx.bitwise_and(eq, old_zf);
        let zf = get_zf(ctx);
        ctx.copy_to(zf, res);
    } else {
        let zf = get_zf(ctx);
        let eq = ctx.i_compare_equal(dest, zero);
        ctx.copy_to(zf, eq);
    }
    let zero = ctx.ir.const_(0);
    let nf = get_nf(ctx);
    let lt = ctx.i_compare_less(dest, zero);
    ctx.copy_to(nf, lt);
}

/// `SetFPZnFlags`.
pub fn set_fp_zn_flags(ctx: &mut EmitterContext<'_, '_>, dest: Opd, set_cc: bool, fp_type: Inst) {
    if set_cc {
        let mut zero = ctx.ir.const_f(0.0);
        if fp_type == FP64 {
            zero = ctx.fp32_convert_to_fp64(zero);
        }
        let zf = get_zf(ctx);
        let eq = ctx.fp_compare_equal_t(dest, zero, fp_type);
        ctx.copy_to(zf, eq);
        let nf = get_nf(ctx);
        let lt = ctx.fp_compare_less_t(dest, zero, fp_type);
        ctx.copy_to(nf, lt);
    }
}

/// `SetFPZnFlags` with the default `FP32`.
pub fn set_fp_zn_flags32(ctx: &mut EmitterContext<'_, '_>, dest: Opd, set_cc: bool) {
    set_fp_zn_flags(ctx, dest, set_cc, FP32);
}

/// `NegateLong`.
pub fn negate_long(ctx: &mut EmitterContext<'_, '_>, low: Opd, high: Opd) -> (Opd, Opd) {
    let low = ctx.bitwise_not(low);
    let high = ctx.bitwise_not(high);
    let one = ctx.ir.const_(1);
    let (low, carry) = add_with_carry(ctx, low, one);
    let high = ctx.i_add(high, carry);
    (low, high)
}

/// `AddWithCarry`: the result and the carry out.
pub fn add_with_carry(ctx: &mut EmitterContext<'_, '_>, lhs: Opd, rhs: Opd) -> (Opd, Opd) {
    let result = ctx.i_add(lhs, rhs);
    let c = ctx.i_compare_less_unsigned(result, lhs);
    let carry = ctx.i_negate(c);
    (result, carry)
}

/// `GetFPComparison` (InstEmitFloatComparison).
pub fn get_fp_comparison(
    ctx: &mut EmitterContext<'_, '_>,
    cond: FComp,
    a: Opd,
    b: Opd,
    fp_type: Inst,
) -> Result<Opd, String> {
    if cond == FComp::T {
        return Ok(ctx.ir.const_(crate::ir::ops::TRUE));
    }
    if cond == FComp::F {
        return Ok(ctx.ir.const_(crate::ir::ops::FALSE));
    }
    if cond == FComp::Nan || cond == FComp::Num {
        let na = ctx.is_nan_t(a, fp_type);
        let nb = ctx.is_nan_t(b, fp_type);
        let mut res = ctx.bitwise_or(na, nb);
        if cond == FComp::Num {
            res = ctx.bitwise_not(res);
        }
        return Ok(res);
    }
    let inst = match cond.0 & !FComp::Nan.0 {
        1 => Inst::CompareLess,
        2 => Inst::CompareEqual,
        3 => Inst::CompareLessOrEqual,
        4 => Inst::CompareGreater,
        5 => Inst::CompareNotEqual,
        6 => Inst::CompareGreaterOrEqual,
        _ => return Err(format!("Unexpected condition \"{}\".", cond.0)),
    };
    let dest = ctx.ir.local();
    let mut res = ctx.add(inst | fp_type, Some(dest), &[a, b]).unwrap_or(dest);
    if cond.0 & FComp::Nan.0 != 0 {
        let na = ctx.is_nan_t(a, fp_type);
        res = ctx.bitwise_or(res, na);
        let nb = ctx.is_nan_t(b, fp_type);
        res = ctx.bitwise_or(res, nb);
    }
    Ok(res)
}
