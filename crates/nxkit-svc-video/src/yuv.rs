//! NV12 to RGBA on the CPU, for a game that reads a picture as RGBA bytes or has the picture written into a texture
//! memory that the host reads.

use crate::frame::Nv12;

/// The coefficients of one `nv12-colorspace` in 16-bit fixed point.
struct Matrix {
    y: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
    y0: i32,
}

impl Matrix {
    /// `colorspace` is the `nv12-colorspace` of the SDK: 1 BT.601 limited, 2 BT.601 full, 3 BT.709 limited, 4 BT.709 full.
    fn new(colorspace: u32) -> Matrix {
        let full = matches!(colorspace, 2 | 4);
        let b709 = matches!(colorspace, 3 | 4);
        let ky = if full { 1.0 } else { 255.0 / 219.0 };
        let kc = if full { 1.0 } else { 255.0 / 224.0 };
        let (rv, gu, gv, bu) = if b709 {
            (1.5748, 0.1873, 0.4681, 1.8556)
        } else {
            (1.402, 0.3441, 0.7141, 1.772)
        };
        let fix = |v: f64| (v * 65536.0) as i32;
        Matrix {
            y: fix(ky),
            rv: fix(rv * kc),
            gu: fix(gu * kc),
            gv: fix(gv * kc),
            bu: fix(bu * kc),
            y0: if full { 0 } else { 16 },
        }
    }
}

/// Converts the picture `src` of `src_w` by `src_h` pixels to RGBA with `dst_w * 4` bytes a row, `dst_h` rows. A smaller
/// picture leaves opaque black around it. A larger one is cropped. `dst` must hold `dst_w * dst_h * 4` bytes.
pub(crate) fn nv12_to_rgba(
    src: &Nv12,
    src_w: usize,
    src_h: usize,
    colorspace: u32,
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
) {
    let need = dst_w * dst_h * 4;
    let Some(dst) = dst.get_mut(..need) else {
        return;
    };
    let (cw, ch) = (src_w.min(dst_w), src_h.min(dst_h));
    if cw < dst_w || ch < dst_h {
        for px in dst.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 0, 255]);
        }
    }
    let m = Matrix::new(colorspace);
    let clamp = |v: i32| v.clamp(0, 255) as u8;
    for row in 0..ch {
        let (Some(y_row), Some(uv_row), Some(out)) = (
            src.y.get(row * src.y_stride..),
            src.uv.get((row >> 1) * src.uv_stride..),
            dst.get_mut(row * dst_w * 4..(row + 1) * dst_w * 4),
        ) else {
            return;
        };
        for x in 0..cw {
            let (Some(&y), Some(&u), Some(&v)) = (
                y_row.get(x),
                uv_row.get(2 * (x >> 1)),
                uv_row.get(2 * (x >> 1) + 1),
            ) else {
                break;
            };
            let l = (i32::from(y) - m.y0) * m.y;
            let (u, v) = (i32::from(u) - 128, i32::from(v) - 128);
            let r = (l + m.rv * v + 32768) >> 16;
            let g = (l - m.gu * u - m.gv * v + 32768) >> 16;
            let b = (l + m.bu * u + 32768) >> 16;
            if let Some(px) = out.get_mut(4 * x..4 * x + 4) {
                px.copy_from_slice(&[clamp(r), clamp(g), clamp(b), 255]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(y: u8, u: u8, v: u8) -> Nv12 {
        Nv12 {
            y: vec![y; 4],
            y_stride: 2,
            uv: vec![u, v],
            uv_stride: 2,
        }
    }

    #[test]
    fn limited_range_black_and_white() {
        let mut out = [0u8; 16];
        nv12_to_rgba(&flat(16, 128, 128), 2, 2, 3, &mut out, 2, 2);
        assert_eq!(&out[..4], &[0, 0, 0, 255]);
        nv12_to_rgba(&flat(235, 128, 128), 2, 2, 3, &mut out, 2, 2);
        assert_eq!(&out[..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn full_range_keeps_the_luma() {
        let mut out = [0u8; 16];
        nv12_to_rgba(&flat(100, 128, 128), 2, 2, 4, &mut out, 2, 2);
        assert_eq!(&out[..4], &[100, 100, 100, 255]);
    }

    #[test]
    fn a_smaller_picture_is_padded_with_black() {
        let mut out = [7u8; 4 * 4 * 4];
        nv12_to_rgba(&flat(235, 128, 128), 2, 2, 3, &mut out, 4, 4);
        assert_eq!(&out[..4], &[255, 255, 255, 255]);
        assert_eq!(&out[4 * 4 * 3..4 * 4 * 3 + 4], &[0, 0, 0, 255]);
        assert_eq!(&out[8..12], &[0, 0, 0, 255]);
    }

    #[test]
    fn a_larger_picture_is_cropped() {
        let mut out = [0u8; 4];
        nv12_to_rgba(&flat(235, 128, 128), 2, 2, 3, &mut out, 1, 1);
        assert_eq!(out, [255, 255, 255, 255]);
    }
}
