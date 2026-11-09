//! The model's input from a decoded picture, on the CPU: the game's colour conversion (`convert_bytes` of
//! `nxkit-ndve/src/msl/common.metal`) written as the model's input `[1, 4, 1080, 1920]`: planar f16, R, G and B as
//! `k / 255`, the fourth plane 1.0.
//!
//! The formula is the kernel's, term by term: the luma plane is read as `Y / 255`, the chroma plane is sampled
//! bilinearly at the pixel centre of the luma grid (clamp to edge), each channel is
//! `fma(V, m2, fma(Y, m0, U * m1))` with the game's matrix and offsets, clamped to 0 to 1, scaled by 255 and rounded half
//! to even, and the byte `k` becomes the f16 of `k / 255`. A chroma texel lies at `x / 2 - 1/4` in texels of its plane,
//! so a pixel's weights are 3/4 and 1/4 in each direction, towards the nearer and the farther texel. The GPU's
//! sampler gives the same weights; its arithmetic may differ from this one by a unit in the last place before the
//! rounding to a byte, so a result can differ by one code where the value lies on a rounding edge.
//!
//! On the CPU rather than on a queue of its own: the picture is already in memory here (the decoder's NV12), nothing
//! has to be uploaded, the GPU the game renders on does no extra pass, and the work runs on the feeder's threads ahead
//! of the game. The loops are plain scalar Rust over rows that the compiler turns into NEON.

use std::sync::OnceLock;

use nxkit_svc_video::Frame;

/// The size of the picture the model takes.
pub(super) const WIDTH: usize = 1920;
pub(super) const HEIGHT: usize = 1080;

/// The colour conversion's constants: the push block of the game's dispatch. Word `4 * row + column` is the matrix
/// (rows give R, G, B; columns weigh Y, U, V), words 12 to 14 are the offsets added to Y, U and V.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Params([u32; 15]);

impl Params {
    /// The bytes of the push block, at least 60 of them. `None` when it is shorter.
    pub(super) fn from_push(bytes: &[u8]) -> Option<Self> {
        let words = bytes.get(..60)?;
        let mut w = [0u32; 15];
        for (v, c) in w.iter_mut().zip(words.as_chunks::<4>().0) {
            *v = u32::from_le_bytes(*c);
        }
        Some(Self(w))
    }

    fn f(&self, i: usize) -> f32 {
        f32::from_bits(self.0[i])
    }
}

/// The f16 bits of `v` for `0 <= v <= 1`, rounded half to even (the conversion `half(x)` of the kernel).
fn f16_bits(v: f32) -> u16 {
    if v <= 0.0 {
        return 0;
    }
    let b = v.to_bits();
    let exp = (b >> 23) as i32 - 127 + 15;
    let mant = b & 0x7f_ffff;
    // Values from 2^-8 to 1 are normal numbers in f16, which is all this is used for.
    let mut h = ((exp as u32) << 10) | (mant >> 13);
    let rest = mant & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && h & 1 == 1) {
        h += 1;
    }
    h as u16
}

/// The f16 bits of `k / 255` for every byte `k`.
fn lut() -> &'static [u16; 256] {
    static LUT: OnceLock<[u16; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u16; 256];
        for (k, v) in t.iter_mut().enumerate() {
            *v = f16_bits(k as f32 / 255.0);
        }
        t
    })
}

/// The bytes `k` of R, G and B of one pixel, from `Y` and the chroma sums `u` and `v` (weights in sixteenths, so
/// `u = 16 * 255 * U`).
#[inline(always)]
fn pixel(p: &Params, y: u8, u: u16, v: u16) -> [u8; 3] {
    let yv = f32::from(y) / 255.0 + p.f(12);
    let uv = f32::from(u) / 4080.0 + p.f(13);
    let vv = f32::from(v) / 4080.0 + p.f(14);
    let ch = |r: usize| {
        let c = vv.mul_add(p.f(r + 2), yv.mul_add(p.f(r), uv * p.f(r + 1)));
        (c.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8
    };
    [ch(0), ch(4), ch(8)]
}

/// The weighted sum of the four chroma texels around a pixel, in sixteenths: 9, 3, 3 and 1 for the nearest texel,
/// the nearest across, the nearest down and the one that is both. One component of the interleaved pair at `c`.
#[inline(always)]
fn chroma_sum(near: &[u8], far: &[u8], x: usize, c: usize) -> u16 {
    let n = near.len() / 2;
    let k = x / 2;
    // The farther texel across: the one to the left for an even pixel, to the right for an odd one.
    let o = if x.is_multiple_of(2) {
        k.saturating_sub(1)
    } else {
        (k + 1).min(n - 1)
    };
    let t = |row: &[u8], i: usize| u16::from(row[2 * i + c]);
    9 * t(near, k) + 3 * t(near, o) + 3 * t(far, k) + t(far, o)
}

/// The chroma rows of a luma row: the one the row lies in, and the farther one, above for an even row and below for an
/// odd one (clamped to the plane).
#[inline(always)]
fn chroma_rows(row: usize) -> (usize, usize) {
    let n = row / 2;
    let far = if row.is_multiple_of(2) {
        n.saturating_sub(1)
    } else {
        (n + 1).min(HEIGHT / 2 - 1)
    };
    (n, far)
}

/// Converts the rows of a band that starts at row `first`. `out` holds the band of each of the four planes (R, G, B and
/// the constant 1.0), `pitch` bytes a row.
fn convert_rows(
    planes: (&[u8], usize, &[u8], usize),
    p: &Params,
    first: usize,
    out: [&mut [u8]; 4],
    pitch: usize,
) {
    let (y, ys, uv, uvs) = planes;
    let lut = lut();
    let one = f16_bits(1.0).to_le_bytes();
    let [r, g, b, a] = out;
    let rows = r.len() / pitch;
    let cw = WIDTH / 2;
    for i in 0..rows {
        let row = first + i;
        let yr = &y[row * ys..row * ys + WIDTH];
        let (cn, cf) = chroma_rows(row);
        let near = &uv[cn * uvs..cn * uvs + 2 * cw];
        let far = &uv[cf * uvs..cf * uvs + 2 * cw];
        let o = i * pitch;
        for (x, &luma) in yr.iter().enumerate() {
            let k = pixel(
                p,
                luma,
                chroma_sum(near, far, x, 0),
                chroma_sum(near, far, x, 1),
            );
            let at = o + 2 * x..o + 2 * x + 2;
            r[at.clone()].copy_from_slice(&lut[usize::from(k[0])].to_le_bytes());
            g[at.clone()].copy_from_slice(&lut[usize::from(k[1])].to_le_bytes());
            b[at.clone()].copy_from_slice(&lut[usize::from(k[2])].to_le_bytes());
            a[at].copy_from_slice(&one);
        }
    }
}

/// Why a picture cannot be converted.
pub(super) fn check(frame: &Frame) -> Result<(), String> {
    if frame.width() as usize == WIDTH && frame.height() as usize == HEIGHT {
        Ok(())
    } else {
        Err(format!(
            "a {}x{} picture, the model takes {WIDTH}x{HEIGHT}",
            frame.width(),
            frame.height()
        ))
    }
}

/// Writes the model's input for `frame` into `tensor` (all four planes). The tensor's rows are `pitch` bytes.
pub(super) fn convert(
    frame: &Frame,
    p: &Params,
    tensor: &mut [u8],
    pitch: usize,
    threads: usize,
) -> Result<(), String> {
    check(frame)?;
    if pitch < WIDTH * 2 || tensor.len() < 4 * HEIGHT * pitch {
        return Err("the input tensor is smaller than the picture".into());
    }
    frame.with_planes(|y, ys, uv, uvs| {
        if ys < WIDTH
            || uvs < WIDTH
            || y.len() < (HEIGHT - 1) * ys + WIDTH
            || uv.len() < (HEIGHT / 2 - 1) * uvs + WIDTH
        {
            return Err("the planes of the picture are too short".to_string());
        }
        let mut tp = tensor[..4 * HEIGHT * pitch].chunks_exact_mut(HEIGHT * pitch);
        let (Some(r), Some(g), Some(b), Some(a)) = (tp.next(), tp.next(), tp.next(), tp.next())
        else {
            return Err("the input tensor has no four planes".to_string());
        };
        // Bands of even rows, one band for each thread, so a band's chroma rows are its own neighbours' too.
        let band = (HEIGHT.div_ceil(threads.max(1)) + 1) & !1;
        std::thread::scope(|s| {
            let chunks = r
                .chunks_mut(band * pitch)
                .zip(g.chunks_mut(band * pitch))
                .zip(b.chunks_mut(band * pitch))
                .zip(a.chunks_mut(band * pitch));
            for (n, (((r, g), b), a)) in chunks.enumerate() {
                s.spawn(move || convert_rows((y, ys, uv, uvs), p, n * band, [r, g, b, a], pitch));
            }
        });
        Ok(())
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_svc_video::Nv12;

    /// The kernel's formula for the pixel `(x, y)`, written the way the kernel reads: the chroma plane is sampled
    /// bilinearly at `(p + 0.5) / size` with texel centres at half texels, clamped to the edge.
    fn reference(planes: &Nv12, p: &Params, x: usize, y: usize) -> [u8; 3] {
        let (cw, ch) = (WIDTH / 2, HEIGHT / 2);
        let texel = |tx: i64, ty: i64, c: usize| {
            let tx = tx.clamp(0, cw as i64 - 1) as usize;
            let ty = ty.clamp(0, ch as i64 - 1) as usize;
            f64::from(planes.uv[ty * planes.uv_stride + 2 * tx + c]) / 255.0
        };
        // The sample position in texels of the chroma plane, minus the half texel to the centre of the first one.
        let sx = (x as f64 + 0.5) / WIDTH as f64 * cw as f64 - 0.5;
        let sy = (y as f64 + 0.5) / HEIGHT as f64 * ch as f64 - 0.5;
        let (x0, y0) = (sx.floor(), sy.floor());
        let (fx, fy) = (sx - x0, sy - y0);
        let sample = |c: usize| {
            let (x0, y0) = (x0 as i64, y0 as i64);
            let top = texel(x0, y0, c) * (1.0 - fx) + texel(x0 + 1, y0, c) * fx;
            let bottom = texel(x0, y0 + 1, c) * (1.0 - fx) + texel(x0 + 1, y0 + 1, c) * fx;
            (top * (1.0 - fy) + bottom * fy) as f32
        };
        let yv = f32::from(planes.y[y * planes.y_stride + x]) / 255.0 + p.f(12);
        let uv = sample(0) + p.f(13);
        let vv = sample(1) + p.f(14);
        let ch = |r: usize| {
            let c = vv.mul_add(p.f(r + 2), yv.mul_add(p.f(r), uv * p.f(r + 1)));
            (c.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8
        };
        [ch(0), ch(4), ch(8)]
    }

    /// The pixel as the loops of the conversion make it.
    fn fast(planes: &Nv12, p: &Params, x: usize, y: usize) -> [u8; 3] {
        let (cn, cf) = chroma_rows(y);
        let row = |r: usize| &planes.uv[r * planes.uv_stride..r * planes.uv_stride + WIDTH];
        let (near, far) = (row(cn), row(cf));
        pixel(
            p,
            planes.y[y * planes.y_stride + x],
            chroma_sum(near, far, x, 0),
            chroma_sum(near, far, x, 1),
        )
    }

    /// BT.709 limited range as the game would push it, in the block's layout.
    fn bt709_limited() -> Params {
        let (ky, kv, ku) = (255.0 / 219.0, 255.0 / 224.0, 255.0 / 224.0);
        let m: [[f32; 3]; 3] = [
            [ky, 0.0, 1.5748 * kv],
            [ky, -0.1873 * ku, -0.4681 * kv],
            [ky, 1.8556 * ku, 0.0],
        ];
        let mut w = [0f32; 15];
        for (r, row) in m.iter().enumerate() {
            w[4 * r..4 * r + 3].copy_from_slice(row);
        }
        w[12] = -16.0 / 255.0;
        w[13] = -128.0 / 255.0;
        w[14] = -128.0 / 255.0;
        let bytes: Vec<u8> = w.iter().flat_map(|f| f.to_le_bytes()).collect();
        Params::from_push(&bytes).unwrap_or(Params([0; 15]))
    }

    /// A picture with gradients and noise in every plane, so that every weight and every edge is visited.
    fn picture() -> Nv12 {
        let mut s = 0x1234_5678u32;
        let mut next = move || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as u8
        };
        let y = (0..WIDTH * HEIGHT)
            .map(|i| ((i % WIDTH) / 8 + (i / WIDTH) / 5) as u8 ^ (next() >> 5))
            .collect();
        let uv = (0..WIDTH * HEIGHT / 2)
            .map(|i| ((i / 7) as u8).wrapping_add(next() >> 4))
            .collect();
        Nv12 {
            y,
            y_stride: WIDTH,
            uv,
            uv_stride: WIDTH,
        }
    }

    #[test]
    fn the_f16_of_a_byte_code_is_the_rounded_half_float() {
        // Known values: 1, 1/2, 1/4, and 1/3 which rounds up (0x3555).
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(0.5), 0x3800);
        assert_eq!(f16_bits(0.25), 0x3400);
        assert_eq!(f16_bits(1.0 / 3.0), 0x3555);
        assert_eq!(f16_bits(0.0), 0);
        let t = lut();
        assert_eq!((t[0], t[255]), (0, 0x3c00));
        // Monotonic, and each entry decodes to within half an f16 step of k / 255.
        for k in 1..256usize {
            assert!(t[k] > t[k - 1]);
            let h = t[k];
            let (e, m) = (i32::from(h >> 10) - 15, f64::from(h & 0x3ff));
            let v = (1.0 + m / 1024.0) * 2f64.powi(e);
            let want = k as f64 / 255.0;
            assert!(
                (v - want).abs() <= 2f64.powi(e - 11) * 1.0001,
                "{k}: {v} {want}"
            );
        }
    }

    #[test]
    fn the_push_block_is_read_as_words() {
        assert!(Params::from_push(&[0; 59]).is_none());
        let p = bt709_limited();
        assert!((p.f(0) - 255.0 / 219.0).abs() < 1e-6);
        assert!((p.f(14) + 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(p.f(3), 0.0);
    }

    #[test]
    fn the_cpu_conversion_is_the_kernels_formula() {
        let (planes, p) = (picture(), bt709_limited());
        // Every pixel of a few rows (the first, an even and an odd one, the last two) and a stride of the others.
        let rows = [0usize, 1, 2, 3, 500, 501, HEIGHT - 2, HEIGHT - 1];
        let (mut checked, mut off) = (0u32, 0u32);
        for y in (0..HEIGHT).filter(|y| rows.contains(y) || y % 37 == 0) {
            for x in 0..WIDTH {
                let (got, want) = (fast(&planes, &p, x, y), reference(&planes, &p, x, y));
                for c in 0..3 {
                    let d = got[c].abs_diff(want[c]);
                    assert!(
                        d <= 1,
                        "({x}, {y}) channel {c}: {} against {}",
                        got[c],
                        want[c]
                    );
                    off += u32::from(d != 0);
                }
                checked += 1;
            }
        }
        // Only values on a rounding edge may differ, by one code: not three in a thousand.
        assert!(off * 1000 < checked * 3, "{off} of {checked} pixels differ");
    }

    #[test]
    fn a_whole_picture_is_written_as_four_planes_at_the_pitch() {
        let p = bt709_limited();
        let planes = picture();
        let frame = Frame::from_nv12(WIDTH as u32, HEIGHT as u32, 3, planes.clone());
        let pitch = WIDTH * 2 + 64;
        let mut tensor = vec![0xeeu8; 4 * HEIGHT * pitch];
        assert_eq!(convert(&frame, &p, &mut tensor, pitch, 3), Ok(()));
        let lut = lut();
        let at = |plane: usize, x: usize, y: usize| {
            let o = (plane * HEIGHT + y) * pitch + 2 * x;
            u16::from_le_bytes([tensor[o], tensor[o + 1]])
        };
        // The band edges (3 threads: bands of 360 rows) and the corners.
        for (x, y) in [
            (0, 0),
            (1, 359),
            (1919, 360),
            (7, 720),
            (1919, 1079),
            (958, 541),
        ] {
            let k = fast(&planes, &p, x, y);
            for c in 0..3 {
                assert_eq!(at(c, x, y), lut[usize::from(k[c])], "({x}, {y}) plane {c}");
            }
            assert_eq!(at(3, x, y), 0x3c00);
        }
        // The padding of a row is left alone.
        assert_eq!(tensor[WIDTH * 2], 0xee);
    }

    #[test]
    fn other_sizes_and_short_tensors_are_refused() {
        let p = bt709_limited();
        let small = Frame::from_nv12(
            16,
            16,
            3,
            Nv12 {
                y: vec![0; 256],
                y_stride: 16,
                uv: vec![0; 128],
                uv_stride: 16,
            },
        );
        assert!(convert(&small, &p, &mut [], 0, 1).is_err());
        let ok = Frame::from_nv12(WIDTH as u32, HEIGHT as u32, 3, picture());
        assert!(convert(&ok, &p, &mut [0; 100], WIDTH * 2, 1).is_err());
    }

    #[test]
    fn gray_stays_gray_and_the_ends_clamp() {
        let p = bt709_limited();
        // Mid grey with neutral chroma: R = G = B, whatever the weights are.
        let k = pixel(&p, 126, 128 * 16, 128 * 16);
        assert_eq!(k[0], k[1]);
        assert_eq!(k[1], k[2]);
        // Limited-range black and white sit at 16 and 235.
        assert_eq!(pixel(&p, 16, 128 * 16, 128 * 16), [0, 0, 0]);
        assert_eq!(pixel(&p, 235, 128 * 16, 128 * 16), [255, 255, 255]);
        assert_eq!(pixel(&p, 0, 128 * 16, 128 * 16), [0, 0, 0]);
        assert_eq!(pixel(&p, 255, 128 * 16, 128 * 16), [255, 255, 255]);
    }
}
