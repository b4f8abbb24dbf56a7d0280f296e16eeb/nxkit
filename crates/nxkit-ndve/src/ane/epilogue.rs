//! The GPU's epilogue of the Neural Engine path on the CPU (`docs/plans/ndve-ane.md`, steps 1 to 5): the reference for
//! the kernel `ndve_ane_epilogue` and the reader of the game's push block that both use.

use crate::half;

/// Words of the resampler's push block (84 bytes).
pub const PUSH_WORDS: usize = 21;

/// What the epilogue takes from the game's resampler push block (`docs/plans/ndve-native.md` section 8).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Epilogue {
    /// Word 10: the size of an output pixel in uv, x.
    pub pixel_w: f32,
    /// Word 11: the size of an output pixel in uv, y.
    pub pixel_h: f32,
    /// Word 18: the row to flip about.
    pub flip_row: i32,
    /// Word 19 is not zero: flip the rows.
    pub flip: bool,
    /// Word 20 is not zero: quantise with the dither.
    pub quant: bool,
}

impl Epilogue {
    /// Reads the block. `None` when it has fewer than [`PUSH_WORDS`] words.
    #[must_use]
    pub fn from_push(push: &[u8]) -> Option<Self> {
        let word = |i: usize| -> Option<u32> {
            let b = push.get(4 * i..4 * i + 4)?;
            Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let _ = word(PUSH_WORDS - 1)?;
        Some(Self {
            pixel_w: f32::from_bits(word(10)?),
            pixel_h: f32::from_bits(word(11)?),
            flip_row: word(18)? as i32,
            flip: word(19)? != 0,
            quant: word(20)? != 0,
        })
    }
}

/// The 2560x1440 RGBA8 image (rows top to bottom as the game's image, `4 * 2560 * 1440` bytes) from `fin` (`final`,
/// f16 bits, `[3][1080][1920]`), `delta` (f16 bits, `[1440][2560]`) and `dither(x, y)` (the dither texture read at the
/// pixel masked with `0x1ff`, three values in `[0, 1]`).
///
/// The sampler is modelled in f32 with exact weights (the GPU's has 8 fractional bits), so a comparison allows one
/// step of the 8-bit result.
#[must_use]
pub fn reference(
    fin: &[u16],
    delta: &[u16],
    dither: &dyn Fn(usize, usize) -> [f32; 3],
    e: &Epilogue,
) -> Vec<u8> {
    const W: usize = 1920;
    const H: usize = 1080;
    const OW: usize = 2560;
    const OH: usize = 1440;
    // The RGB10 picture: round(final * 1023) / 1023, half to even.
    let q = |c: usize, x: usize, y: usize| -> f32 {
        let v = half::to_f32(fin[c * W * H + y * W + x]).clamp(0.0, 1.0);
        (v * 1023.0).round_ties_even() / 1023.0
    };
    let mut out = vec![0u8; 4 * OW * OH];
    for y in 0..OH {
        for x in 0..OW {
            let u = (x as f32 + 0.5) * e.pixel_w * W as f32 - 0.5;
            let v = (y as f32 + 0.5) * e.pixel_h * H as f32 - 0.5;
            let (x0, y0) = (u.floor(), v.floor());
            let (fx, fy) = (u - x0, v - y0);
            let at = |p: f32, max: usize| (p.max(0.0) as usize).min(max - 1);
            let (xa, xb, ya, yb) = (at(x0, W), at(x0 + 1.0, W), at(y0, H), at(y0 + 1.0, H));
            let d = half::to_f32(delta[y * OW + x]);
            let yo = if e.flip {
                (e.flip_row - y as i32) as usize
            } else {
                y
            };
            let mut px = [0u8; 4];
            for (c, out_c) in px.iter_mut().take(3).enumerate() {
                let top = q(c, xa, ya) * (1.0 - fx) + q(c, xb, ya) * fx;
                let bot = q(c, xa, yb) * (1.0 - fx) + q(c, xb, yb) * fx;
                let s = half::to_f32(half::from_f32(top * (1.0 - fy) + bot * fy));
                let mut r = half::to_f32(half::from_f32(s + d));
                if e.quant {
                    let dv = dither(x & 511, y & 511)[c];
                    r = (r.mul_add(255.0, dv * 2.0 - 1.0)).round_ties_even() / 255.0;
                }
                *out_c = (r.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8;
            }
            px[3] = 255;
            let o = 4 * (yo * OW + x);
            out[o..o + 4].copy_from_slice(&px);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(w10: f32, w11: f32, flip_row: u32, flip: u32, quant: u32) -> Vec<u8> {
        let mut words = [0u32; PUSH_WORDS];
        words[10] = w10.to_bits();
        words[11] = w11.to_bits();
        words[18] = flip_row;
        words[19] = flip;
        words[20] = quant;
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn the_push_block_is_read_by_word() {
        let p = push(1.0 / 2560.0, 1.0 / 1440.0, 1439, 1, 0);
        let e = Epilogue::from_push(&p);
        assert_eq!(
            e,
            Some(Epilogue {
                pixel_w: 1.0 / 2560.0,
                pixel_h: 1.0 / 1440.0,
                flip_row: 1439,
                flip: true,
                quant: false
            })
        );
        assert_eq!(Epilogue::from_push(&p[..80]), None);
    }

    #[test]
    fn a_flat_picture_plus_delta_flips_and_quantises() {
        let one = half::from_f32(0.5);
        let fin = vec![one; 3 * 1920 * 1080];
        let mut delta = vec![half::ZERO; 2560 * 1440];
        delta[0] = half::from_f32(0.25);
        let e = Epilogue::from_push(&push(1.0 / 2560.0, 1.0 / 1440.0, 1439, 1, 0)).unwrap_or(
            Epilogue {
                pixel_w: 0.0,
                pixel_h: 0.0,
                flip_row: 0,
                flip: false,
                quant: false,
            },
        );
        let img = reference(&fin, &delta, &|_, _| [0.0; 3], &e);
        // Pixel (0, 0) lands in the last row; 0.7507 -> 191.
        let o = 4 * (1439 * 2560);
        assert_eq!(&img[o..o + 4], &[191, 191, 191, 255]);
        // Elsewhere 0.5 is 512/1023 in RGB10, 127.6 of 255 -> 128.
        assert_eq!(&img[0..4], &[128, 128, 128, 255]);
    }
}
