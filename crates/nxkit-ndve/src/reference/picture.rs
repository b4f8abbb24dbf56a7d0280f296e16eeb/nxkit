//! The picture steps of the reference: the colour conversion, the RGB10A2 packing and the resampler.

use super::{Dither, Error, Image, Nv12, Picture10, step};
use crate::half::{self, NEG_ZERO, ZERO};
use crate::net::{RESAMPLER_BLOCK, RESAMPLER_LEN};

pub(super) fn pack10(v: [f32; 4]) -> u32 {
    let q = |x: f32, max: f32| (x.clamp(0.0, 1.0) * max).round_ties_even() as u32;
    q(v[0], 1023.0) | (q(v[1], 1023.0) << 10) | (q(v[2], 1023.0) << 20) | (q(v[3], 3.0) << 30)
}

fn unpack10(p: u32) -> [f32; 3] {
    [
        (p & 0x3ff) as f32 / 1023.0,
        ((p >> 10) & 0x3ff) as f32 / 1023.0,
        ((p >> 20) & 0x3ff) as f32 / 1023.0,
    ]
}

pub(super) fn unorm8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8
}

// ---------------------------------------------------------------------------------------------------------------
// Colour conversion

/// A bilinear sample at the unnormalised texel position (`x`, `y`), clamp to edge. The filter is in f32; the hardware
/// uses fixed-point weights (R: the difference is below an 8-bit step).
fn bilinear<const N: usize>(
    fetch: impl Fn(usize, usize) -> [f32; N],
    x: f32,
    y: f32,
    w: usize,
    h: usize,
) -> [f32; N] {
    let (px, py) = (x - 0.5, y - 0.5);
    let (fx0, fy0) = (px.floor(), py.floor());
    let (fx, fy) = (px - fx0, py - fy0);
    let clamp = |v: f32, n: usize| (v as i64).clamp(0, n as i64 - 1) as usize;
    let (x0, x1) = (clamp(fx0, w), clamp(fx0 + 1.0, w));
    let (y0, y1) = (clamp(fy0, h), clamp(fy0 + 1.0, h));
    let (a, b, c, d) = (fetch(x0, y0), fetch(x1, y0), fetch(x0, y1), fetch(x1, y1));
    let mut r = [0.0f32; N];
    for i in 0..N {
        let top = a[i] * (1.0 - fx) + b[i] * fx;
        let bottom = c[i] * (1.0 - fx) + d[i] * fx;
        r[i] = top * (1.0 - fy) + bottom * fy;
    }
    r
}

/// The colour conversion of pass 3 (section 7): NV12 to an RGBA8 picture.
///
/// `push` holds the 16 words of the push block as floats: matrix rows at words 0 to 2, 4 to 6 and 8 to 10, offsets at
/// words 12 to 14 (words 3, 7, 11 and 15 are unused). With `Y' = Y + w12`, `U' = Cb + w13`, `V' = Cr + w14`:
/// `r = fma(V', w2, fma(Y', w0, U' * w1))` and likewise for g (words 4 to 6) and b (words 8 to 10), all f32. The
/// chroma is sampled bilinearly at `uv = ((x + 0.5) / W, (y + 0.5) / H)` (clamp to edge, a guess: R). Alpha is 1.
/// The unorm write rounds to nearest even (inferred).
pub fn convert_nv12(nv12: &Nv12, push: &[f32; 16]) -> Result<Image, Error> {
    let (w, h) = (nv12.width, nv12.height);
    if w == 0 || h == 0 || !w.is_multiple_of(8) || !h.is_multiple_of(8) {
        return Err(Error::Shape("the picture size must be a multiple of 8"));
    }
    if nv12.luma.len() != w * h || nv12.chroma.len() != (w / 2) * (h / 2) * 2 {
        return Err(Error::Length("the planes do not match the picture size"));
    }
    let (cw, ch) = (w / 2, h / 2);
    let mut data = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let yy = f32::from(nv12.luma[y * w + x]) / 255.0 + push[12];
            let u = (x as f32 + 0.5) / w as f32;
            let v = (y as f32 + 0.5) / h as f32;
            let c = bilinear(
                |sx, sy| {
                    let at = (sy * cw + sx) * 2;
                    [
                        f32::from(nv12.chroma[at]) / 255.0,
                        f32::from(nv12.chroma[at + 1]) / 255.0,
                    ]
                },
                u * cw as f32,
                v * ch as f32,
                cw,
                ch,
            );
            let (cb, cr) = (c[0] + push[13], c[1] + push[14]);
            let rgb = [
                cr.mul_add(push[2], yy.mul_add(push[0], cb * push[1])),
                cr.mul_add(push[6], yy.mul_add(push[4], cb * push[5])),
                cr.mul_add(push[10], yy.mul_add(push[8], cb * push[9])),
            ];
            let at = (y * w + x) * 4;
            for (i, &c) in rgb.iter().enumerate() {
                data[at + i] = unorm8(c);
            }
        }
    }
    Ok(Image {
        width: w,
        height: h,
        data,
    })
}

// ---------------------------------------------------------------------------------------------------------------
// Resampler

/// The weights of one resampler block as f32 values of halves.
struct Block {
    /// `w1[g][m * 16 + k]`: the 16x16 matrix `W1_g`, stored as four 8x8 row-major tiles in the order `[kb][rb]`.
    w1: [[f32; 256]; 2],
    /// `w2[g][j][k * 8 + n]`: the 16x8 tile `W2_gj` of two 8x8 row-major sub-tiles (`k` 0 to 7, then 8 to 15).
    w2: [[[f32; 128]; 2]; 2],
}

fn parse_block(bytes: &[u8]) -> Block {
    let h = |off: usize| half::to_f32(u16::from_le_bytes([bytes[off], bytes[off + 1]]));
    let mut b = Block {
        w1: [[0.0; 256]; 2],
        w2: [[[0.0; 128]; 2]; 2],
    };
    for g in 0..2 {
        for kb in 0..2 {
            for rb in 0..2 {
                for i in 0..8 {
                    for j in 0..8 {
                        let off = g * 0x200 + (kb * 2 + rb) * 0x80 + i * 16 + j * 2;
                        b.w1[g][(8 * rb + i) * 16 + 8 * kb + j] = h(off);
                    }
                }
            }
        }
        for jj in 0..2 {
            for s in 0..2 {
                for i in 0..8 {
                    for n in 0..8 {
                        let off = 0x400 + 0x200 * g + 0x100 * jj + s * 0x80 + i * 16 + n * 2;
                        b.w2[g][jj][(8 * s + i) * 8 + n] = h(off);
                    }
                }
            }
        }
    }
    b
}

/// The resampler MLP on one 16x16 luma window `x` (`x[row * 16 + col]`, halves as f32). Returns the 8x8 delta
/// (`delta[row * 8 + col]`, half bit patterns).
///
/// Six blocks. In each: `H_g = W1_g . X` for g = 0, 1 (as two 16x8 column halves, one HMMA each: 4 HMMAs), then
/// `Y_j = H_0 . W2_0j + H_1 . W2_1j` for j = 0, 1 (a chain of two HMMAs, g = 0 first, starting from C = 0), and
/// `X = max(Y, 0)` for the next block. The last block computes only j = 0 and has no ReLU: its rows 0 to 7 are the
/// delta (inferred: the notes say "the 8x8 delta" of a 16x8 result, not which rows).
/// Every HMMA is a [`hmma_step`]-ordered sum with the rounding to half.
fn mlp(blocks: &[Block; 6], x: &[f32; 256]) -> [u16; 64] {
    let mut x = *x;
    let mut delta = [ZERO; 64];
    for (bi, blk) in blocks.iter().enumerate() {
        let last = bi == 5;
        // Stage 1.
        let mut hh = [[0.0f32; 256]; 2];
        for (h, w1) in hh.iter_mut().zip(&blk.w1) {
            for m in 0..16 {
                let a = &w1[m * 16..m * 16 + 16];
                for col in 0..16 {
                    h[m * 16 + col] = half::to_f32(step(a, &x, col, 16, ZERO));
                }
            }
        }
        // Stage 2.
        let mut next = [0.0f32; 256];
        for j in 0..if last { 1 } else { 2 } {
            for m in 0..16 {
                for n in 0..8 {
                    let d0 = step(&hh[0][m * 16..m * 16 + 16], &blk.w2[0][j], n, 8, ZERO);
                    let d1 = step(&hh[1][m * 16..m * 16 + 16], &blk.w2[1][j], n, 8, d0);
                    if last {
                        if m < 8 {
                            delta[m * 8 + n] = d1;
                        }
                    } else {
                        next[m * 16 + 8 * j + n] = half::to_f32(half::relu(d1));
                    }
                }
            }
        }
        x = next;
    }
    delta
}

/// The resampler (pass 16, section 8): the RGB10A2 picture to the RGBA8 output, 4/3 larger.
///
/// `push` is the 21 words of the push block (words 2 and 3 are the integer maximum x and y of the luma window, 10 and
/// 11 the floats 1/2560 and 1/1440, 18 the flip base, 19 the flip enable, 20 the quantise and dither enable).
/// `weights` are the 0x3000 bytes of the six blocks. `dither` is used when word 20 is not zero; without a texture the
/// dither value is 0.5, which adds nothing.
///
/// The output is `8 * ceil(w / 6)` by `8 * ceil(h / 6)` (2560x1440 for 1920x1080). Output block (`bx`, `by`) of 8x8
/// pixels uses the luma window of 16x16 pixels at (`6 bx - 5`, `6 by - 5`), each coordinate clamped to
/// `[0, w2]` x `[0, w3]` (the window mapping is inferred from the PR: I). Window element `X[row][col]` is the pixel at
/// (`x0 + col`, `y0 + row`). Luma is `fma(B, 0.0722, fma(R, 0.2126, G * 0.7152))` in f32, rounded to half. Each output
/// pixel is `(-0.0) + (s + delta)` per channel in half, with `s` the picture sampled bilinearly at
/// `((x + 0.5) * w10, (y + 0.5) * w11)` and rounded to half (the TEX unit returns half); with `w20 != 0` then
/// `k = rint(fma(v, 255, dither * 2 - 1))` and `v = k * (1/255)`; alpha is 1; the row is `w18 - y` when `w19 != 0`.
pub fn resample(
    picture: &Picture10,
    weights: &[u8],
    push: &[u32; 21],
    dither: Option<&Dither>,
) -> Result<Image, Error> {
    if weights.len() != RESAMPLER_LEN {
        return Err(Error::Length(
            "the resampler's weights are not 0x3000 bytes",
        ));
    }
    let (pw, ph) = (picture.width, picture.height);
    if pw == 0 || ph == 0 || picture.data.len() != pw * ph {
        return Err(Error::Shape("the picture data does not match its size"));
    }
    let blocks: [Block; 6] = std::array::from_fn(|i| {
        parse_block(&weights[i * RESAMPLER_BLOCK..(i + 1) * RESAMPLER_BLOCK])
    });
    let luma: Vec<u16> = picture
        .data
        .iter()
        .map(|&p| {
            let [r, g, b] = unpack10(p);
            half::from_f32(b.mul_add(0.0722, r.mul_add(0.2126, g * 0.7152)))
        })
        .collect();
    let max_x = i64::from(push[2]);
    let max_y = i64::from(push[3]);
    let (w10, w11) = (f32::from_bits(push[10]), f32::from_bits(push[11]));
    let quantise = push[20] != 0;
    let flip = push[19] != 0;
    let (bxs, bys) = (pw.div_ceil(6), ph.div_ceil(6));
    let (ow, oh) = (bxs * 8, bys * 8);
    let mut out = Image {
        width: ow,
        height: oh,
        data: vec![0; ow * oh * 4],
    };
    for by in 0..bys {
        for bx in 0..bxs {
            let mut win = [0.0f32; 256];
            for row in 0..16 {
                let wy = (6 * by as i64 - 5 + row as i64).clamp(0, max_y.min(ph as i64 - 1).max(0))
                    as usize;
                for col in 0..16 {
                    let wx = (6 * bx as i64 - 5 + col as i64)
                        .clamp(0, max_x.min(pw as i64 - 1).max(0))
                        as usize;
                    win[row * 16 + col] = half::to_f32(luma[wy * pw + wx]);
                }
            }
            let delta = mlp(&blocks, &win);
            for py in 0..8 {
                for px in 0..8 {
                    let (x, y) = (bx * 8 + px, by * 8 + py);
                    let u = (x as f32 + 0.5) * w10;
                    let v = (y as f32 + 0.5) * w11;
                    let s = bilinear(
                        |sx, sy| unpack10(picture.data[sy * pw + sx]),
                        u * pw as f32,
                        v * ph as f32,
                        pw,
                        ph,
                    );
                    let yo = if flip {
                        i64::from(push[18]) - y as i64
                    } else {
                        y as i64
                    };
                    if yo < 0 || yo >= oh as i64 {
                        continue;
                    }
                    let at = (yo as usize * ow + x) * 4;
                    for (c, &sc) in s.iter().enumerate() {
                        let sum = half::add(half::from_f32(sc), delta[py * 8 + px]);
                        let mut val = half::to_f32(half::add(NEG_ZERO, sum));
                        if quantise {
                            let d = dither.map_or(0.5, |t| {
                                t.values[(y & 0x1ff) % t.height * t.width + (x & 0x1ff) % t.width]
                            });
                            let k = val.mul_add(255.0, d * 2.0 - 1.0).round_ties_even();
                            val = k * f32::from_bits(0x3b80_8081);
                        }
                        out.data[at + c] = unorm8(val);
                    }
                    out.data[at + 3] = 255;
                }
            }
        }
    }
    Ok(out)
}
