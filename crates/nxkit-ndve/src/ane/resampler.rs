//! The resampler's weights (`0x3000` bytes, six blocks of `0x800`) as the convolution weights the ANE graph needs.
//!
//! A block is two stages on a 16x16 window `X[row][col]` of luma: `H_g = W1_g . X` (g = 0, 1), then
//! `Y_j = H_0 . W2_0j + H_1 . W2_1j` (j = 0, 1). `W1_g` is 16x16 stored as four 8x8 tiles in the order `[kb][rb]`,
//! `W2_gj` is 16x8 stored as two 8x8 tiles `[s]` (row `8 s + i`); all tiles hold f16 row-major. Block 0 reads the
//! luma window straight from the picture with two strided-conv equivalents (a `3x1` then a grouped `1x3` convolution
//! over the 6x6 space-to-depth of the padded luma); blocks 1 to 5 read the previous block's 16x16 outputs as 256
//! channels and are folded into one dense 1x1 convolution each: `W[(m, j, n)][(row, col)] = sum_g W1_g[m][row] *
//! W2_gj[col][n]`, summed in f32 and rounded to f16 once.

use crate::half;

/// Halves in a block.
const BLOCK_HALVES: usize = 0x400;

/// The little-endian f16 bit patterns of `b` (an odd last byte is dropped).
pub(super) fn halves(b: &[u8]) -> Vec<u16> {
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}

/// One block's weights.
pub(super) struct Block {
    /// `[g][m][r]`, 2 x 16 x 16.
    w1: Vec<u16>,
    /// `[g][j][c][n]`, 2 x 2 x 16 x 8.
    w2: Vec<u16>,
}

impl Block {
    fn w1(&self, g: usize, m: usize, r: usize) -> u16 {
        self.w1[(g * 16 + m) * 16 + r]
    }

    fn w2(&self, g: usize, j: usize, c: usize, n: usize) -> u16 {
        self.w2[((g * 2 + j) * 16 + c) * 8 + n]
    }
}

/// The six blocks of the resampler weights `res`.
pub(super) fn blocks(res: &[u8]) -> Result<Vec<Block>, String> {
    if res.len() != crate::net::RESAMPLER_LEN {
        return Err(format!(
            "{} resampler bytes, expected {}",
            res.len(),
            crate::net::RESAMPLER_LEN
        ));
    }
    let h = halves(res);
    let mut out = Vec::with_capacity(6);
    for bi in 0..6 {
        let base = bi * BLOCK_HALVES;
        let mut w1 = vec![0u16; 2 * 16 * 16];
        let mut w2 = vec![0u16; 2 * 2 * 16 * 8];
        for g in 0..2 {
            for kb in 0..2 {
                for rb in 0..2 {
                    let t = base + (g * 0x200 + (kb * 2 + rb) * 0x80) / 2;
                    for i in 0..8 {
                        for j in 0..8 {
                            w1[(g * 16 + 8 * rb + i) * 16 + 8 * kb + j] = h[t + i * 8 + j];
                        }
                    }
                }
            }
            for j in 0..2 {
                for s in 0..2 {
                    let t = base + (0x400 + 0x200 * g + 0x100 * j + s * 0x80) / 2;
                    for i in 0..8 {
                        for n in 0..8 {
                            w2[((g * 2 + j) * 16 + 8 * s + i) * 8 + n] = h[t + i * 8 + n];
                        }
                    }
                }
            }
        }
        out.push(Block { w1, w2 });
    }
    Ok(out)
}

/// Block 0's first convolution `[192][36][3][1]`: output `m * 12 + g * 6 + dx`, input `dy * 6 + dx`, tap `a` is the
/// luma row `6 a + dy` of the window.
pub(super) fn block0_stage1(b: &Block) -> Vec<u16> {
    let mut w = vec![0u16; 192 * 36 * 3];
    for m in 0..16 {
        for g in 0..2 {
            for dx in 0..6 {
                for a in 0..3 {
                    for dy in 0..6 {
                        let r = 6 * a + dy;
                        if r < 16 {
                            let o = m * 12 + g * 6 + dx;
                            w[(o * 36 + dy * 6 + dx) * 3 + a] = b.w1(g, m, r);
                        }
                    }
                }
            }
        }
    }
    w
}

/// Block 0's second convolution `[256][12][1][3]`, 16 groups: output `m * 16 + j * 8 + n`, input `g * 6 + dx`, tap `b`
/// is the window column `6 b + dx`.
pub(super) fn block0_stage2(b: &Block) -> Vec<u16> {
    let mut w = vec![0u16; 256 * 12 * 3];
    for m in 0..16 {
        for j in 0..2 {
            for n in 0..8 {
                for g in 0..2 {
                    for dx in 0..6 {
                        for t in 0..3 {
                            let c = 6 * t + dx;
                            if c < 16 {
                                let o = m * 16 + j * 8 + n;
                                w[(o * 12 + g * 6 + dx) * 3 + t] = b.w2(g, j, c, n);
                            }
                        }
                    }
                }
            }
        }
    }
    w
}

/// The dense 1x1 weight `[ms * js * 8][256]` of block `bi` (1 to 5): `ms = 16, js = 2` for blocks 1 to 4, `ms = 8,
/// js = 1` for block 5, which has no ReLU. Returns the weight and its output channels.
pub(super) fn dense(b: &Block, last: bool) -> (Vec<u16>, usize) {
    let (ms, js) = if last { (8, 1) } else { (16, 2) };
    let mut w = Vec::with_capacity(ms * js * 8 * 256);
    for m in 0..ms {
        for j in 0..js {
            for n in 0..8 {
                for r in 0..16 {
                    for c in 0..16 {
                        let mut s = 0f32;
                        for g in 0..2 {
                            s += half::to_f32(b.w1(g, m, r)) * half::to_f32(b.w2(g, j, c, n));
                        }
                        w.push(half::from_f32(s));
                    }
                }
            }
        }
    }
    (w, ms * js * 8)
}
