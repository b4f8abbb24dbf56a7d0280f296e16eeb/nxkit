//! The arithmetic of the model: f32 operations with denormals flushed (the SASS `.FTZ`), f16 rounding, and the HMMA.
//!
//! Halves are carried as the `f32` of their value, so every half operation reads `h(x)` for a rounding to half.

use crate::half;

/// Flush a denormal to a zero of the same sign.
pub fn ftz(x: f32) -> f32 {
    if x != 0.0 && x.abs() < f32::MIN_POSITIVE {
        0.0f32.copysign(x)
    } else {
        x
    }
}

/// `a * b`, inputs and result flushed.
pub fn mul(a: f32, b: f32) -> f32 {
    ftz(ftz(a) * ftz(b))
}

/// `a + b`, inputs and result flushed.
pub fn add(a: f32, b: f32) -> f32 {
    ftz(ftz(a) + ftz(b))
}

/// `a * b + c` with one rounding, inputs and result flushed.
pub fn fma(a: f32, b: f32, c: f32) -> f32 {
    ftz(ftz(a).mul_add(ftz(b), ftz(c)))
}

/// `1 / x`.
pub fn rcp(x: f32) -> f32 {
    1.0 / x
}

/// A double rounded to a half, as the `f32` of its value.
pub fn h(x: f64) -> f32 {
    half::to_f32(half::from_f64(x))
}

/// `max(x, +0)` as the model writes it: a negative value becomes +0, -0 and NaN stay.
pub fn relu(x: f32) -> f32 {
    if x < 0.0 { 0.0 } else { x }
}

/// A 16x16 matrix of halves (as `f32`).
pub type Mat = [[f32; 16]; 16];

/// `D[r][n] = h(sum_k A[r][k] B[k][n] + C[r][n])`: the HMMA, one rounding of the exact sum.
///
/// The sum of 16 products and `C` is exact in a 128-bit integer of units 2^-48 (a product of two halves is a
/// multiple of 2^-48 below 2^32). The model rounds that sum to a double and the double to a half, as Python's `fsum`
/// and `struct` do; an exact zero is +0. Non-finite operands fall back to a plain double sum.
pub fn hmma(a: &Mat, b: &Mat, c: Option<&Mat>) -> Mat {
    let fixed = |x: f32| (f64::from(x) * 16_777_216.0) as i64; // units 2^-24, exact for a half
    let fa: Vec<[i64; 16]> = a.iter().map(|r| r.map(fixed)).collect();
    let fb: Vec<[i64; 16]> = b.iter().map(|r| r.map(fixed)).collect();
    let finite = a
        .iter()
        .chain(b)
        .chain(c.iter().flat_map(|m| m.iter()))
        .all(|r| r.iter().all(|x| x.is_finite()));
    let scale = f64::from_bits(((1023 - 48) as u64) << 52);
    let mut d = [[0.0f32; 16]; 16];
    for r in 0..16 {
        for n in 0..16 {
            let cv = c.map_or(0.0, |c| c[r][n]);
            d[r][n] = if finite {
                let mut s: i128 = i128::from(fixed(cv)) << 24;
                for k in 0..16 {
                    s += i128::from(fa[r][k]) * i128::from(fb[k][n]);
                }
                h(s as f64 * scale)
            } else {
                let s: f64 = (0..16)
                    .map(|k| f64::from(a[r][k]) * f64::from(b[k][n]))
                    .sum();
                h(s + f64::from(cv))
            };
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eye(v: f32) -> Mat {
        let mut m = [[0.0; 16]; 16];
        for (i, r) in m.iter_mut().enumerate() {
            r[i] = v;
        }
        m
    }

    #[test]
    fn denormals_are_flushed() {
        let tiny = f32::MIN_POSITIVE / 2.0;
        assert_eq!(ftz(tiny).to_bits(), 0);
        assert_eq!(ftz(-tiny).to_bits(), (-0.0f32).to_bits());
        assert_eq!(mul(tiny, 2.0), 0.0);
        assert_eq!(add(tiny, 0.0), 0.0);
        assert_eq!(fma(2.0, 3.0, 1.0), 7.0);
        // The product is below the normal range: flushed.
        assert_eq!(fma(f32::MIN_POSITIVE, 0.5, 0.0), 0.0);
    }

    #[test]
    fn relu_keeps_negative_zero_and_nan() {
        assert_eq!(relu(-1.0).to_bits(), 0);
        assert_eq!(relu(-0.0).to_bits(), (-0.0f32).to_bits());
        assert!(relu(f32::NAN).is_nan());
        assert_eq!(relu(2.0), 2.0);
    }

    #[test]
    fn hmma_identity_and_accumulator() {
        let mut x = [[0.0; 16]; 16];
        for (i, r) in x.iter_mut().enumerate() {
            for (j, v) in r.iter_mut().enumerate() {
                *v = (i as f32 - 8.0) * 0.25 + j as f32 * 0.0625;
            }
        }
        assert_eq!(hmma(&eye(1.0), &x, None), x);
        let two = hmma(&eye(1.0), &x, Some(&x));
        assert_eq!(two[3][4], 2.0 * x[3][4]);
    }

    #[test]
    fn hmma_rounds_the_whole_sum_once() {
        // 2048 + 1 + 1 in one sum is 2050 (exact); rounding after each term would stay at 2048 (ties to even).
        let mut a = [[0.0; 16]; 16];
        let mut b = [[0.0; 16]; 16];
        a[0][0] = 2048.0;
        a[0][1] = 1.0;
        a[0][2] = 1.0;
        b[0][0] = 1.0;
        b[1][0] = 1.0;
        b[2][0] = 1.0;
        assert_eq!(hmma(&a, &b, None)[0][0], 2050.0);
        // Cancellation keeps the small term.
        a[0][0] = 1024.0;
        a[0][1] = -1024.0;
        a[0][2] = 2.0f32.powi(-20);
        assert_eq!(hmma(&a, &b, None)[0][0], 2.0f32.powi(-20));
        // An exact zero is +0, an underflow keeps its sign.
        a[0][2] = 0.0;
        assert_eq!(hmma(&a, &b, None)[0][0].to_bits(), 0);
        a[0][0] = -2.0f32.powi(-14);
        a[0][1] = 0.0;
        a[0][2] = 0.0;
        b[0][0] = 2.0f32.powi(-14);
        assert_eq!(hmma(&a, &b, None)[0][0].to_bits(), (-0.0f32).to_bits());
    }

    #[test]
    fn hmma_overflow_is_infinity() {
        let mut a = [[0.0; 16]; 16];
        a[0][0] = 60000.0;
        a[0][1] = 60000.0;
        let b = eye(1.0);
        let mut b2 = b;
        b2[1][0] = 1.0;
        assert_eq!(hmma(&a, &b2, None)[0][0], f32::INFINITY);
    }
}
