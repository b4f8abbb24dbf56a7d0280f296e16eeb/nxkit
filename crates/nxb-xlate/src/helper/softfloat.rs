//! The soft float64 helpers of `HelperFunctionManager` (`ConvertDoubleToFloat`, `ConvertFloatToDouble`) used by the
//! `DoubleToFloat` pass when the host has no float64 support.
use super::{B, Function};
use crate::ir::{Inst, Ir, Opd, StorageKind};

type R<T> = Result<T, String>;

fn k(b: &mut B, v: i32) -> Opd {
    b.ir.const_(v)
}

fn bin(b: &mut B, inst: Inst, x: Opd, c: i32) -> Opd {
    let c = k(b, c);
    b.op(inst, &[x, c])
}

pub(super) fn convert_double_to_float(ir: &mut Ir) -> R<Function> {
    let mut b = B::new(ir);
    let low = b.ir.argument(0);
    let high = b.ir.argument(1);
    let mantissa_low = bin(&mut b, Inst::BitwiseAnd, low, (1 << 22) - 1);
    let mut mantissa = bin(&mut b, Inst::ShiftRightU32, low, 22);
    let t = bin(&mut b, Inst::BitwiseAnd, high, 0xfffff);
    let t = bin(&mut b, Inst::ShiftLeft, t, 10);
    mantissa = b.op(Inst::BitwiseOr, &[mantissa, t]);
    let (c1, c0) = (k(&mut b, 1), k(&mut b, 0));
    let sel = b.op(Inst::ConditionalSelect, &[mantissa_low, c1, c0]);
    mantissa = b.op(Inst::BitwiseOr, &[mantissa, sel]);
    let e = bin(&mut b, Inst::ShiftRightU32, high, 20);
    let mut exp = bin(&mut b, Inst::BitwiseAnd, e, 0x7ff);
    let sign = bin(&mut b, Inst::ShiftRightS32, high, 31);
    let result_sign = bin(&mut b, Inst::ShiftLeft, sign, 31);
    let not_zero = b.op(Inst::BitwiseOr, &[mantissa, exp]);
    let lbl = b.ir.label();
    b.branch_if_true(lbl, not_zero);
    b.ret(Some(result_sign));
    b.mark_label(lbl);
    let not_nan_or_inf = bin(&mut b, Inst::CompareNotEqual, exp, 0x7ff);
    mantissa = bin(&mut b, Inst::BitwiseOr, mantissa, 0x4000_0000);
    exp = bin(&mut b, Inst::Subtract, exp, 0x381);
    let round_bits = bin(&mut b, Inst::BitwiseAnd, mantissa, 0x7f);
    let round_bits_xor64 = bin(&mut b, Inst::BitwiseExclusiveOr, round_bits, 0x40);
    let added = bin(&mut b, Inst::Add, mantissa, 0x40);
    mantissa = bin(&mut b, Inst::ShiftRightU32, added, 7);
    let (a, c) = (k(&mut b, !0), k(&mut b, !1));
    let sel = b.op(Inst::ConditionalSelect, &[round_bits_xor64, a, c]);
    mantissa = b.op(Inst::BitwiseAnd, &[mantissa, sel]);
    let c0 = k(&mut b, 0);
    exp = b.op(Inst::ConditionalSelect, &[mantissa, exp, c0]);
    let cff = k(&mut b, 0xff);
    exp = b.op(Inst::ConditionalSelect, &[not_nan_or_inf, exp, cff]);
    let sh = bin(&mut b, Inst::ShiftLeft, exp, 23);
    let sum = b.op(Inst::Add, &[mantissa, sh]);
    let result = b.op(Inst::Add, &[sum, result_sign]);
    b.ret(Some(result));
    b.finish_with("ConvertDoubleToFloat", true, 2, 0)
}

pub(super) fn convert_float_to_double(ir: &mut Ir) -> R<Function> {
    let mut b = B::new(ir);
    let value = b.ir.argument(0);
    let mut mantissa = bin(&mut b, Inst::BitwiseAnd, value, 0x7f_ffff);
    let e = bin(&mut b, Inst::ShiftRightU32, value, 23);
    let mut exp = bin(&mut b, Inst::BitwiseAnd, e, 0xff);
    let sign = bin(&mut b, Inst::ShiftRightS32, value, 31);
    let not_nan_or_inf = bin(&mut b, Inst::CompareNotEqual, exp, 0xff);
    let exp_not_zero = bin(&mut b, Inst::CompareNotEqual, exp, 0);
    let m_zero = bin(&mut b, Inst::CompareEqual, mantissa, 0);
    let not_denorm = b.op(Inst::BitwiseOr, &[exp_not_zero, m_zero]);
    exp = bin(&mut b, Inst::Add, exp, 0x380);
    let msb = b.op(Inst::FindMSBU32, &[mantissa]);
    let c32 = k(&mut b, 32);
    let shift_dist = b.op(Inst::Subtract, &[c32, msb]);
    let c1 = k(&mut b, 1);
    let t = b.op(Inst::Subtract, &[c1, shift_dist]);
    let norm_exp = bin(&mut b, Inst::Subtract, t, 1);
    let norm_mant = b.op(Inst::ShiftLeft, &[mantissa, shift_dist]);
    let c7ff = k(&mut b, 0x7ff);
    exp = b.op(Inst::ConditionalSelect, &[not_nan_or_inf, exp, c7ff]);
    exp = b.op(Inst::ConditionalSelect, &[not_denorm, exp, norm_exp]);
    mantissa = b.op(
        Inst::ConditionalSelect,
        &[exp_not_zero, mantissa, norm_mant],
    );
    let result_low = bin(&mut b, Inst::ShiftLeft, mantissa, 29);
    let mut result_high = bin(&mut b, Inst::ShiftRightU32, mantissa, 3);
    let sh = bin(&mut b, Inst::ShiftLeft, exp, 20);
    result_high = b.op(Inst::Add, &[result_high, sh]);
    let sh = bin(&mut b, Inst::ShiftLeft, sign, 31);
    result_high = b.op(Inst::Add, &[result_high, sh]);
    let (a1, a2) = (b.ir.argument(1), b.ir.argument(2));
    b.raw(Inst::Copy, StorageKind::None, Some(a1), &[result_low]);
    b.raw(Inst::Copy, StorageKind::None, Some(a2), &[result_high]);
    b.ret(None);
    b.finish_with("ConvertFloatToDouble", false, 1, 2)
}
