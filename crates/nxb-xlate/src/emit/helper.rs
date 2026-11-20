//! `InstEmitHelper.cs`: operand accessors shared by the instruction emitters.

use crate::emit::emitter::EmitterContext;
use crate::ir::ops::{Opd, regtype};
use crate::maxwell::enums::{HalfSwizzle, OFmt};

/// `GetZF`.
pub fn get_zf(ctx: &mut EmitterContext<'_, '_>) -> Opd {
    ctx.ir.register(0, regtype::FLAG)
}

/// `GetNF`.
pub fn get_nf(ctx: &mut EmitterContext<'_, '_>) -> Opd {
    ctx.ir.register(1, regtype::FLAG)
}

/// `GetCF`.
pub fn get_cf(ctx: &mut EmitterContext<'_, '_>) -> Opd {
    ctx.ir.register(2, regtype::FLAG)
}

/// `GetVF`.
pub fn get_vf(ctx: &mut EmitterContext<'_, '_>) -> Opd {
    ctx.ir.register(3, regtype::FLAG)
}

/// `GetDest`.
pub fn get_dest(ctx: &mut EmitterContext<'_, '_>, rd: i32) -> Opd {
    ctx.ir.register(rd, regtype::GPR)
}

/// `GetDest2`.
pub fn get_dest2(ctx: &mut EmitterContext<'_, '_>, rd: i32) -> Opd {
    ctx.ir.register(rd | 1, regtype::GPR)
}

/// `GetSrcCbuf`.
pub fn get_src_cbuf(
    ctx: &mut EmitterContext<'_, '_>,
    slot: i32,
    offset: i32,
    is_fp64: bool,
) -> Opd {
    if is_fp64 {
        let a = ctx.ir.cbuf(slot, offset);
        let b = ctx.ir.cbuf(slot, offset + 1);
        ctx.pack_double2x32(a, b)
    } else {
        ctx.ir.cbuf(slot, offset)
    }
}

/// `GetSrcImm`.
pub fn get_src_imm(ctx: &mut EmitterContext<'_, '_>, imm: i32, is_fp64: bool) -> Opd {
    if is_fp64 {
        let z = ctx.ir.const_(0);
        let i = ctx.ir.const_(imm);
        ctx.pack_double2x32(z, i)
    } else {
        ctx.ir.const_(imm)
    }
}

/// `GetSrcReg`.
pub fn get_src_reg(ctx: &mut EmitterContext<'_, '_>, reg: i32, is_fp64: bool) -> Opd {
    if is_fp64 {
        let a = ctx.ir.register(reg, regtype::GPR);
        let b = ctx.ir.register(reg | 1, regtype::GPR);
        ctx.pack_double2x32(a, b)
    } else {
        ctx.ir.register(reg, regtype::GPR)
    }
}

/// `(float)Unsafe.As<ushort, Half>`.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let man = (h & 0x3FF) as u32;
    let bits = if exp == 0 {
        if man == 0 {
            sign << 31
        } else {
            // subnormal half: normalise
            let mut e = 127 - 15 + 1;
            let mut m = man;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            (sign << 31) | ((e as u32) << 23) | ((m & 0x3FF) << 13)
        }
    } else if exp == 0x1F {
        (sign << 31) | (0xFF << 23) | (man << 13)
    } else {
        (sign << 31) | ((exp + 127 - 15) << 23) | (man << 13)
    };
    f32::from_bits(bits)
}

/// `GetHalfSrc(context, swizzle, ra, negate, absolute)`.
pub fn get_half_src_reg(
    ctx: &mut EmitterContext<'_, '_>,
    swizzle: HalfSwizzle,
    ra: i32,
    negate: bool,
    absolute: bool,
) -> Result<[Opd; 2], String> {
    let src = get_src_reg(ctx, ra, false);
    let ops = get_half_unpacked(ctx, src, swizzle)?;
    Ok(fp_abs_neg_pair(ctx, ops, absolute, negate))
}

/// `GetHalfSrc(context, swizzle, cbufSlot, cbufOffset, negate, absolute)`.
pub fn get_half_src_cbuf(
    ctx: &mut EmitterContext<'_, '_>,
    swizzle: HalfSwizzle,
    slot: i32,
    offset: i32,
    negate: bool,
    absolute: bool,
) -> Result<[Opd; 2], String> {
    let src = get_src_cbuf(ctx, slot, offset, false);
    let ops = get_half_unpacked(ctx, src, swizzle)?;
    Ok(fp_abs_neg_pair(ctx, ops, absolute, negate))
}

/// `GetHalfSrc(context, immH0, immH1)`.
pub fn get_half_src_imm10(ctx: &mut EmitterContext<'_, '_>, imm_h0: i32, imm_h1: i32) -> [Opd; 2] {
    let lo = half_to_f32((imm_h0 << 6) as u16);
    let hi = half_to_f32((imm_h1 << 6) as u16);
    [ctx.ir.const_f(lo), ctx.ir.const_f(hi)]
}

/// `GetHalfSrc(context, imm32)`.
pub fn get_half_src_imm32(ctx: &mut EmitterContext<'_, '_>, imm32: i32) -> [Opd; 2] {
    let lo = half_to_f32(imm32 as u16);
    let hi = half_to_f32((imm32 >> 16) as u16);
    [ctx.ir.const_f(lo), ctx.ir.const_f(hi)]
}

/// `FPAbsNeg(context, operands, abs, neg)`.
pub fn fp_abs_neg_pair(
    ctx: &mut EmitterContext<'_, '_>,
    ops: [Opd; 2],
    abs: bool,
    neg: bool,
) -> [Opd; 2] {
    let a = ctx.fp_abs_neg(ops[0], abs, neg);
    let b = ctx.fp_abs_neg(ops[1], abs, neg);
    [a, b]
}

/// `GetHalfUnpacked`.
pub fn get_half_unpacked(
    ctx: &mut EmitterContext<'_, '_>,
    src: Opd,
    swizzle: HalfSwizzle,
) -> Result<[Opd; 2], String> {
    Ok(match swizzle {
        HalfSwizzle::F16 => {
            let l = ctx.unpack_half2x16_low(src);
            let h = ctx.unpack_half2x16_high(src);
            [l, h]
        }
        HalfSwizzle::F32 => [src, src],
        HalfSwizzle::H0H0 => {
            let l = ctx.unpack_half2x16_low(src);
            let l2 = ctx.unpack_half2x16_low(src);
            [l, l2]
        }
        HalfSwizzle::H1H1 => {
            let h = ctx.unpack_half2x16_high(src);
            let h2 = ctx.unpack_half2x16_high(src);
            [h, h2]
        }
        _ => return Err(format!("Invalid swizzle \"{}\".", swizzle.0)),
    })
}

/// `GetHalfPacked`.
pub fn get_half_packed(
    ctx: &mut EmitterContext<'_, '_>,
    swizzle: OFmt,
    results: [Opd; 2],
    rd: i32,
) -> Result<Opd, String> {
    match swizzle {
        OFmt::F16 => Ok(ctx.pack_half2x16(results[0], results[1])),
        OFmt::F32 => Ok(results[0]),
        OFmt::MrgH0 => {
            let h1 = get_half_dest(ctx, rd, true);
            Ok(ctx.pack_half2x16(results[0], h1))
        }
        OFmt::MrgH1 => {
            let h0 = get_half_dest(ctx, rd, false);
            Ok(ctx.pack_half2x16(h0, results[1]))
        }
        _ => Err(format!("Invalid swizzle \"{}\".", swizzle.0)),
    }
}

/// `GetHalfDest`.
pub fn get_half_dest(ctx: &mut EmitterContext<'_, '_>, rd: i32, is_high: bool) -> Opd {
    let d = get_dest(ctx, rd);
    if is_high {
        ctx.unpack_half2x16_high(d)
    } else {
        ctx.unpack_half2x16_low(d)
    }
}

/// `GetPredicate`.
pub fn get_predicate(ctx: &mut EmitterContext<'_, '_>, pred: i32, not: bool) -> Opd {
    let local = ctx.ir.register(pred, regtype::PREDICATE);
    if not { ctx.bitwise_not(local) } else { local }
}

/// `SetDest`.
pub fn set_dest(ctx: &mut EmitterContext<'_, '_>, value: Opd, rd: i32, is_fp64: bool) {
    if is_fp64 {
        let d = get_dest(ctx, rd);
        let lo = ctx.unpack_double2x32_low(value);
        ctx.copy_to(d, lo);
        let d2 = get_dest2(ctx, rd);
        let hi = ctx.unpack_double2x32_high(value);
        ctx.copy_to(d2, hi);
    } else {
        let d = get_dest(ctx, rd);
        ctx.copy_to(d, value);
    }
}

/// `Imm16ToSInt`.
pub fn imm16_to_sint(imm16: i32) -> i32 {
    imm16 as i16 as i32
}

/// `Imm20ToFloat`.
pub fn imm20_to_float(imm20: i32) -> i32 {
    imm20 << 12
}

/// `Imm20ToSInt`.
pub fn imm20_to_sint(imm20: i32) -> i32 {
    (imm20 << 12) >> 12
}

/// `Imm24ToSInt`.
pub fn imm24_to_sint(imm24: i32) -> i32 {
    (imm24 << 8) >> 8
}

/// `SignExtendTo32`.
pub fn sign_extend_to32(ctx: &mut EmitterContext<'_, '_>, src: Opd, src_bits: i32) -> Opd {
    let z = ctx.ir.const_(0);
    let b = ctx.ir.const_(src_bits);
    ctx.bitfield_extract_s32(src, z, b)
}

/// `ZeroExtendTo32`.
pub fn zero_extend_to32(ctx: &mut EmitterContext<'_, '_>, src: Opd, src_bits: i32) -> Opd {
    let mask = (u32::MAX >> (32 - src_bits)) as i32;
    let m = ctx.ir.const_(mask);
    ctx.bitwise_and(src, m)
}

#[cfg(test)]
mod tests {
    use super::half_to_f32;

    #[test]
    fn half_conversion() {
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x7C00), f32::INFINITY);
        assert!(half_to_f32(0x7E00).is_nan());
        assert_eq!(half_to_f32(0x0001), 2.0f32.powi(-24));
        assert_eq!(half_to_f32(0x0400), 2.0f32.powi(-14));
    }
}
