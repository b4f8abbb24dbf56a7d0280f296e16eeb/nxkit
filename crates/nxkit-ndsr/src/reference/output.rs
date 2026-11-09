//! The output pixels of a tile (sections 7.6 to 7.8): the tile rule, the resample, the chroma, the encoding, the dither.

use super::arith::{add, fma, mul, rcp};
use super::grid::Grid;
use super::picture::Texture;
use super::{Pixel, Push};
use crate::net::{Net, Variant, VrConsts};

fn k(bits: u32) -> f32 {
    f32::from_bits(bits)
}

/// First output pixel of input tile `wi` along one axis: `((2 step wi + 1) n_out + n_in - 1) div (2 n_in)`.
pub fn tile_start(wi: usize, step: usize, n_in: usize, n_out: usize) -> usize {
    ((2 * step * wi + 1) * n_out + n_in - 1) / (2 * n_in)
}

/// The output range `[start, end)` of tile `wi` along one axis.
pub fn tile_range(wi: usize, step: usize, n_in: usize, n_out: usize) -> (usize, usize) {
    let start = if wi == 0 {
        0
    } else {
        tile_start(wi, step, n_in, n_out)
    };
    let end = if step * (wi + 1) >= n_in {
        n_out
    } else {
        tile_start(wi + 1, step, n_in, n_out)
    };
    (start, end)
}

/// The sRGB encoding of variant A (`FSETP.GEU`: equal and NaN take the power branch).
pub(crate) fn srgb(lin: f32) -> f32 {
    if lin >= k(0x3b4d_2e1c) || lin.is_nan() {
        let lg = if lin > 0.0 {
            (f64::log2(f64::from(lin))) as f32
        } else {
            f32::NEG_INFINITY
        };
        let e = if lg != f32::NEG_INFINITY {
            2.0f64.powf(f64::from(mul(lg, k(0x3ed5_5555)))) as f32
        } else {
            0.0
        };
        fma(e, k(0x3f87_0a3d), k(0xbd61_47ae))
    } else {
        mul(lin, k(0x414e_b852))
    }
}

/// The dither of pixel `(x, y)` before the output offset: a triangular integer in -65535 .. 65535.
pub fn hash(x: u32, y: u32) -> i32 {
    let h1 = x.wrapping_mul(1_597_334_677) ^ y.wrapping_mul(3_812_015_819);
    let h = h1.wrapping_mul(1_597_334_677);
    (h & 0xffff) as i32 - (h >> 16) as i32
}

fn dither(variant: Variant, d: i32, rgb: [f32; 3]) -> [f32; 3] {
    let df = d as f32;
    match variant {
        Variant::A => rgb.map(|o| fma(df, k(0x3380_8102), o)),
        Variant::B => rgb.map(|o| fma(df, k(0x3280_2088), o)),
        Variant::C => {
            let q = mul(df, k(0x3780_0080));
            let mut i = 0;
            rgb.map(|o| {
                let shift = if i < 2 { 6 } else { 5 };
                i += 1;
                let eb = (o.to_bits() & 0x7f80_0000).wrapping_sub(shift << 23);
                let e = f32::from_bits(eb);
                let e = if e.is_nan() || e < 0.0 {
                    0.0
                } else {
                    e.min(1.0)
                };
                fma(q, e, o)
            })
        }
    }
}

/// All pixels of the tile `(wx, wy)`: the sharpened grid `hs` resampled to the output, in rows.
pub fn pixels(
    net: &Net,
    c: &VrConsts,
    push: &Push,
    tex: &dyn Texture,
    hs: &Grid,
    (wx, wy): (usize, usize),
) -> Vec<Pixel> {
    let (bx, by) = (usize::from(net.step_x), usize::from(net.step_y));
    let (win, hin) = (push.w_in as usize, push.h_in as usize);
    let (wout, hout) = (push.w_out as usize, push.h_out as usize);
    let (x0, x1) = tile_range(wx, bx, win, wout);
    let (y0, y1) = tile_range(wy, by, hin, hout);
    let ratio = k(c.ratio_bits);
    let (r_w, r_h) = (rcp(win as f32), rcp(hin as f32));
    let (r_wo, r_ho) = (rcp(wout as f32), rcp(hout as f32));
    let (sx_w, sy_h) = (mul(win as f32, ratio), mul(hin as f32, ratio));
    let (sx, sy) = (mul(sx_w, r_wo), mul(sy_h, r_ho));
    let off = |sc: f32, tile0: usize, s_full: f32, r_in: f32| {
        fma(-mul(tile0 as f32, s_full), r_in, fma(sc, 0.5, -0.5))
    };
    let (off_x, off_y) = (off(sx, bx * wx, sx_w, r_w), off(sy, by * wy, sy_h, r_h));
    let (gw, gh) = (hs.width as i64, hs.height as i64);
    let cell = |i: i64, j: i64| hs.at(i.clamp(0, gh - 1) as usize, j.clamp(0, gw - 1) as usize);
    let mut out = Vec::with_capacity((x1.saturating_sub(x0)) * (y1.saturating_sub(y0)));
    for y in y0..y1 {
        for x in x0..x1 {
            let gx = fma(sx, x as f32, off_x);
            let gy = fma(sy, y as f32, off_y);
            let (ix, iy) = (gx.floor() as i64, gy.floor() as i64);
            let fx = add(gx, -(ix as f32));
            let fy = add(gy, -(iy as f32));
            let (a, b) = (cell(iy, ix), cell(iy, ix + 1));
            let (cc, d) = (cell(iy + 1, ix), cell(iy + 1, ix + 1));
            let top = fma(fx, b, fma(-fx, a, a));
            let bot = fma(fx, d, fma(-fx, cc, cc));
            let v = fma(fy, bot, fma(-fy, top, top));
            let g = (f64::from(if v > 0.0 { v } else { 0.0 }).sqrt()) as f32;
            let u = mul(add(x as f32, 0.5), r_wo);
            let w = mul(add(y as f32, 0.5), r_ho);
            let t = tex.sample(u, w).map(|v| v as f32);
            let rg = add(t[0], -t[1]);
            let bg = add(-t[1], t[2]);
            let r = add(g, rg);
            let bl = add(g, bg);
            let gr = fma(
                g,
                k(0x3fb2_f88e),
                fma(r, k(0xbe98_3266), mul(bl, k(0xbdce_bf49))),
            );
            let mut rgb = [r, gr, bl].map(|c| mul(c, c.abs()));
            if net.variant == Variant::A {
                rgb = rgb.map(srgb);
            }
            if push.dither {
                rgb = dither(net.variant, hash(x as u32, y as u32), rgb);
            }
            out.push(Pixel {
                x: x as i32 + push.offset.0,
                y: y as i32 + push.offset.1,
                rgba: [rgb[0], rgb[1], rgb[2], 1.0],
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_partition_the_output() {
        // For several sizes and tilings the ranges are contiguous from 0 to the output width.
        for (n_in, n_out, step) in [
            (1536, 1920, 39),
            (763, 1920, 27),
            (640, 1920, 31),
            (100, 100, 23),
            (64, 96, 39),
        ] {
            let groups = usize::div_ceil(n_in, step);
            let mut at = 0;
            for w in 0..groups {
                let (s, e) = tile_range(w, step, n_in, n_out);
                assert_eq!(s, at, "{n_in} {n_out} {step} tile {w}");
                assert!(e >= s);
                at = e;
            }
            assert_eq!(at, n_out);
        }
    }

    #[test]
    fn first_and_last_tiles() {
        // 1.2 A at 1536 -> 1920: the first tile starts at 0, the second at round(39 * 1.25 + 0.5) up.
        assert_eq!(tile_range(0, 39, 1536, 1920).0, 0);
        assert_eq!(tile_start(1, 39, 1536, 1920), (79 * 1920 + 1535) / 3072);
        assert_eq!(tile_range(39, 39, 1536, 1920).1, 1920);
    }

    #[test]
    fn hash_is_triangular_and_wraps() {
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1919, 1079), (u32::MAX, 7)] {
            let d = hash(x, y);
            assert!((-65535..=65535).contains(&d), "{d}");
        }
        assert_eq!(hash(0, 0), 0);
        let h1 = 1u32.wrapping_mul(1_597_334_677);
        let h = h1.wrapping_mul(1_597_334_677);
        assert_eq!(hash(1, 0), (h & 0xffff) as i32 - (h >> 16) as i32);
    }

    #[test]
    fn srgb_branches() {
        assert_eq!(srgb(0.0), 0.0);
        assert!((srgb(0.002) - 0.002 * 12.92).abs() < 1e-6);
        assert!((srgb(1.0) - 1.0).abs() < 1e-6);
        assert!((srgb(0.5) - 0.735_357).abs() < 1e-5);
    }

    #[test]
    fn dither_c_scales_with_the_exponent() {
        // One ulp of a 11-bit-mantissa float of 1.0 is 2^-6 of ... the step is the exponent minus 6 (minus 5 for z).
        let rgb = dither(Variant::C, 65535, [1.0, 1.0, 1.0]);
        assert_eq!(rgb[0], 1.0 + 65535.0 * k(0x3780_0080) * 2.0f32.powi(-6));
        assert_eq!(rgb[2], 1.0 + 65535.0 * k(0x3780_0080) * 2.0f32.powi(-5));
    }
}
