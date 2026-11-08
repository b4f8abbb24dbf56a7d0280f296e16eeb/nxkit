//! Software f16 (IEEE 754 binary16) with round to nearest even.
//!
//! The reference needs the exact rounding of the GPU's half arithmetic, so it does not lean on a hardware type.
//! Values are carried as their `u16` bit patterns. Subnormals and signed zeros are kept. A sum of two halves is exact
//! in `f64` (the exponents span at most 40 bits plus 11 bits of mantissa), so [`add`] adds in `f64` and rounds once,
//! which is what a half add or a half fused multiply-add with a multiplier of 1 does.

/// The bit pattern of +0.0.
pub const ZERO: u16 = 0x0000;
/// The bit pattern of -0.0.
pub const NEG_ZERO: u16 = 0x8000;
/// The bit pattern of 1.0.
pub const ONE: u16 = 0x3c00;

/// The exact value of a half as `f32`. Every half is exactly representable.
pub fn to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let mant = f32::from(h & 0x3ff);
    if exp == 0 {
        // Subnormal: mant * 2^-24.
        sign * mant * f32::from_bits(0x3380_0000)
    } else if exp == 31 {
        if mant == 0.0 {
            sign * f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        // (1024 + mant) * 2^(exp - 25)
        let scale = f32::from_bits(((exp - 25 + 127) as u32) << 23);
        sign * (1024.0 + mant) * scale
    }
}

/// `x` rounded to a half, ties to even. Overflow gives infinity, NaN gives a quiet NaN.
pub fn from_f64(x: f64) -> u16 {
    let sign: u16 = if x.is_sign_negative() { 0x8000 } else { 0 };
    if x.is_nan() {
        return 0x7e00;
    }
    let a = x.abs();
    if a == 0.0 {
        return sign;
    }
    if a.is_infinite() {
        return sign | 0x7c00;
    }
    // `a` is at least 2^-149 in practice (f32 and f16 inputs), so its f64 exponent field is a normal one.
    let e = ((a.to_bits() >> 52) & 0x7ff) as i32 - 1023;
    if e < -14 {
        // Subnormal range: a multiple of 2^-24. A carry into 0x0400 is the smallest normal, which is right.
        let q = (a * pow2(24)).round_ties_even();
        return sign | q as u16;
    }
    let mut e = e;
    let mut r = (a * pow2(10 - e)).round_ties_even() as u32;
    if r == 2048 {
        r = 1024;
        e += 1;
    }
    if e > 15 {
        return sign | 0x7c00;
    }
    sign | (((e + 15) as u16) << 10) | (r - 1024) as u16
}

/// `x` rounded to a half, ties to even.
pub fn from_f32(x: f32) -> u16 {
    from_f64(f64::from(x))
}

/// `a + b` with one rounding to half.
pub fn add(a: u16, b: u16) -> u16 {
    from_f64(f64::from(to_f32(a)) + f64::from(to_f32(b)))
}

/// `max(h, 0)` as the ReLU flag of a half fused multiply-add does it: a negative value (and -0.0) becomes +0.0.
/// This is inferred (I in ndve-native.md): the code only shows the flag, not what it does to -0.0 or NaN.
/// A NaN passes through.
pub fn relu(h: u16) -> u16 {
    let nan = (h & 0x7c00) == 0x7c00 && (h & 0x03ff) != 0;
    if h & 0x8000 != 0 && !nan { ZERO } else { h }
}

/// 2^n as `f64`, for n in -1022..=1023.
fn pow2(n: i32) -> f64 {
    f64::from_bits(((n + 1023) as u64) << 52)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(from_f32(1.0), 0x3c00);
        assert_eq!(from_f32(-2.0), 0xc000);
        assert_eq!(from_f32(0.5), 0x3800);
        assert_eq!(from_f32(65504.0), 0x7bff);
        assert_eq!(from_f32(65520.0), 0x7c00);
        assert_eq!(from_f32(1.0 / 3.0), 0x3555);
        assert_eq!(to_f32(0x3555), 0.333_251_95);
        assert_eq!(to_f32(0x7bff), 65504.0);
        assert_eq!(to_f32(0xc000), -2.0);
    }

    #[test]
    fn zeros_and_subnormals() {
        assert_eq!(from_f32(0.0), 0x0000);
        assert_eq!(from_f32(-0.0), 0x8000);
        assert!(to_f32(0x8000).is_sign_negative());
        assert_eq!(to_f32(0x0001), 2.0f32.powi(-24));
        assert_eq!(from_f32(2.0f32.powi(-24)), 0x0001);
        assert_eq!(to_f32(0x0400), 2.0f32.powi(-14));
        // Half of the smallest subnormal is a tie and goes to even (zero); a hair more rounds up.
        assert_eq!(from_f32(2.0f32.powi(-25)), 0x0000);
        assert_eq!(from_f32(2.0f32.powi(-25) * 1.001), 0x0001);
        // The largest subnormal plus a half ulp rounds up into the smallest normal.
        assert_eq!(from_f32(1023.5 * 2.0f32.powi(-24)), 0x0400);
        assert_eq!(from_f32(-1.0e-9), 0x8000);
    }

    #[test]
    fn ties_go_to_even() {
        // 1 + 2^-11 is halfway between 1.0 (even) and 1 + 2^-10.
        assert_eq!(from_f32(1.0 + 2.0f32.powi(-11)), 0x3c00);
        // 1 + 3 * 2^-11 is halfway between 1 + 2^-10 (odd) and 1 + 2^-9 (even).
        assert_eq!(from_f32(1.0 + 3.0 * 2.0f32.powi(-11)), 0x3c02);
    }

    #[test]
    fn every_half_round_trips() {
        for h in 0..=u16::MAX {
            let nan = (h & 0x7c00) == 0x7c00 && (h & 0x3ff) != 0;
            if nan {
                assert!(to_f32(h).is_nan());
            } else {
                assert_eq!(from_f32(to_f32(h)), h, "{h:#06x}");
            }
        }
    }

    #[test]
    fn add_rounds_once() {
        assert_eq!(add(0x3c00, 0x3c00), 0x4000);
        // 2048 + 1 = 2049 is a tie between 2048 and 2050 (spacing 2) and goes to even.
        assert_eq!(add(from_f32(2048.0), 0x3c00), from_f32(2048.0));
        assert_eq!(add(NEG_ZERO, NEG_ZERO), NEG_ZERO);
        assert_eq!(add(ZERO, NEG_ZERO), ZERO);
    }

    #[test]
    fn relu_clears_negatives() {
        assert_eq!(relu(0xc000), ZERO);
        assert_eq!(relu(NEG_ZERO), ZERO);
        assert_eq!(relu(0x4000), 0x4000);
    }
}
