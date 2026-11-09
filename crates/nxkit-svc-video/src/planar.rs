//! The planar pictures of the software AV1 decoder and their conversion to NV12, the layout that the game reads.

use crate::frame::Nv12;

/// The chroma layout of a planar picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// No chroma planes.
    Mono,
    /// Chroma halved across and down.
    I420,
    /// Chroma halved across.
    I422,
    /// Chroma at full size.
    I444,
}

/// A planar picture, as the software decoder gives it: rows of samples of one byte (8 bits) or two bytes (more, little
/// endian), each plane with its stride in bytes.
pub(crate) struct Planar<'a> {
    pub width: usize,
    pub height: usize,
    pub bit_depth: u8,
    pub layout: Layout,
    /// Luma, Cb and Cr with their strides in bytes. Cb and Cr are empty for `Layout::Mono`.
    pub planes: [(&'a [u8], usize); 3],
}

impl Planar<'_> {
    /// The 8-bit sample at `x` in a row of a plane. Samples of more bits drop the low bits. A missing sample is the
    /// middle grey, so a short plane gives a flat picture, not a panic.
    fn sample(&self, plane: usize, row: usize, x: usize) -> u8 {
        let (data, stride) = self.planes[plane];
        let wide = self.bit_depth > 8;
        let at = row * stride + x * (1 + usize::from(wide));
        if wide {
            match data.get(at..at + 2) {
                Some(b) => (u16::from_le_bytes([b[0], b[1]]) >> (self.bit_depth - 8)) as u8,
                None => 128,
            }
        } else {
            data.get(at).copied().unwrap_or(128)
        }
    }

    /// The picture as NV12: 8 bits, chroma interleaved. A picture with more chroma than 4:2:0 keeps every other row and
    /// column of it. A picture with no chroma gets grey chroma.
    ///
    /// The AV1 worker converts every picture before it sends the next packet, 60 a second for a 1080p movie, so the
    /// common case, 8-bit 4:2:0 with whole planes, copies rows (`to_nv12_rows`). The rest goes sample by sample.
    pub(crate) fn to_nv12(&self) -> Nv12 {
        self.to_nv12_rows()
            .unwrap_or_else(|| self.to_nv12_by_sample())
    }

    /// [`Planar::to_nv12`] for an 8-bit 4:2:0 picture: luma rows are copied and the chroma rows interleaved. `None` for
    /// another layout or depth, or a plane shorter than the picture.
    fn to_nv12_rows(&self) -> Option<Nv12> {
        if self.bit_depth != 8 || self.layout != Layout::I420 {
            return None;
        }
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let row = |plane: usize, r: usize, len: usize| -> Option<&[u8]> {
            let (data, stride) = self.planes[plane];
            data.get(r * stride..r * stride + len)
        };
        let mut y = Vec::with_capacity(w * h);
        for r in 0..h {
            y.extend_from_slice(row(0, r, w)?);
        }
        let mut uv = vec![0u8; cw * 2 * ch];
        for (r, line) in uv.chunks_exact_mut((cw * 2).max(1)).enumerate() {
            let (u, v) = (row(1, r, cw)?, row(2, r, cw)?);
            for ((pair, &cb), &cr) in line.as_chunks_mut::<2>().0.iter_mut().zip(u).zip(v) {
                pair[0] = cb;
                pair[1] = cr;
            }
        }
        Some(Nv12 {
            y,
            y_stride: w,
            uv,
            uv_stride: cw * 2,
        })
    }

    /// [`Planar::to_nv12`] for any picture, one sample at a time.
    fn to_nv12_by_sample(&self) -> Nv12 {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut y = Vec::with_capacity(w * h);
        for row in 0..h {
            y.extend((0..w).map(|x| self.sample(0, row, x)));
        }
        let mut uv = Vec::with_capacity(cw * 2 * ch);
        for row in 0..ch {
            for x in 0..cw {
                let (u, v) = match self.layout {
                    Layout::Mono => (128, 128),
                    Layout::I420 => (self.sample(1, row, x), self.sample(2, row, x)),
                    Layout::I422 => (self.sample(1, 2 * row, x), self.sample(2, 2 * row, x)),
                    Layout::I444 => (
                        self.sample(1, 2 * row, 2 * x),
                        self.sample(2, 2 * row, 2 * x),
                    ),
                };
                uv.push(u);
                uv.push(v);
            }
        }
        Nv12 {
            y,
            y_stride: w,
            uv,
            uv_stride: cw * 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_8_bit_420_picture_is_interleaved() {
        let y: Vec<u8> = (0..16).collect();
        let (u, v) = ([100u8, 101, 102, 103], [200u8, 201, 202, 203]);
        let p = Planar {
            width: 4,
            height: 4,
            bit_depth: 8,
            layout: Layout::I420,
            planes: [(&y, 4), (&u, 2), (&v, 2)],
        };
        let n = p.to_nv12();
        assert_eq!(n.y, y);
        assert_eq!(n.uv, vec![100, 200, 101, 201, 102, 202, 103, 203]);
        assert_eq!((n.y_stride, n.uv_stride), (4, 4));
    }

    #[test]
    fn a_10_bit_picture_drops_two_bits() {
        let samples: Vec<u8> = [0x3FFu16, 0x200, 0x100, 0x004]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let chroma: Vec<u8> = 0x200u16.to_le_bytes().to_vec();
        let p = Planar {
            width: 2,
            height: 2,
            bit_depth: 10,
            layout: Layout::I420,
            planes: [(&samples, 4), (&chroma, 2), (&chroma, 2)],
        };
        let n = p.to_nv12();
        assert_eq!(n.y, vec![255, 128, 64, 1]);
        assert_eq!(n.uv, vec![128, 128]);
    }

    #[test]
    fn a_short_plane_gives_grey_and_no_panic() {
        let p = Planar {
            width: 4,
            height: 4,
            bit_depth: 8,
            layout: Layout::I420,
            planes: [(&[], 4), (&[], 2), (&[], 2)],
        };
        let n = p.to_nv12();
        assert_eq!((n.y.len(), n.uv.len()), (16, 8));
        assert!(n.y.iter().chain(&n.uv).all(|&b| b == 128));
    }

    #[test]
    fn mono_and_444_have_chroma_in_nv12() {
        let y = [10u8, 20, 30, 40];
        let m = Planar {
            width: 2,
            height: 2,
            bit_depth: 8,
            layout: Layout::Mono,
            planes: [(&y, 2), (&[], 0), (&[], 0)],
        };
        assert_eq!(m.to_nv12().uv, vec![128, 128]);
        let (u, v) = ([1u8, 2, 3, 4], [5u8, 6, 7, 8]);
        let f = Planar {
            width: 2,
            height: 2,
            bit_depth: 8,
            layout: Layout::I444,
            planes: [(&y, 2), (&u, 2), (&v, 2)],
        };
        assert_eq!(f.to_nv12().uv, vec![1, 5]);
    }

    #[test]
    fn rows_and_samples_give_the_same_nv12() {
        // An odd size and strides wider than the rows, as rav1d pads them.
        let (w, h, ys, cs) = (5usize, 3usize, 8usize, 4usize);
        let y: Vec<u8> = (0..ys * h).map(|i| i as u8).collect();
        let u: Vec<u8> = (0..cs * 2).map(|i| 100 + i as u8).collect();
        let v: Vec<u8> = (0..cs * 2).map(|i| 200 + i as u8).collect();
        let p = Planar {
            width: w,
            height: h,
            bit_depth: 8,
            layout: Layout::I420,
            planes: [(&y, ys), (&u, cs), (&v, cs)],
        };
        let Some(fast) = p.to_nv12_rows() else {
            unreachable!("whole 8-bit 4:2:0 planes take the row copy")
        };
        let slow = p.to_nv12_by_sample();
        assert_eq!((fast.y, fast.uv), (slow.y, slow.uv));
        assert_eq!((fast.y_stride, fast.uv_stride), (5, 6));
    }

    #[test]
    fn a_short_plane_leaves_the_row_copy() {
        let y = [0u8; 16];
        let p = Planar {
            width: 4,
            height: 4,
            bit_depth: 8,
            layout: Layout::I420,
            planes: [(&y, 4), (&[1, 2, 3], 2), (&[4, 5, 6, 7], 2)],
        };
        assert!(p.to_nv12_rows().is_none());
        assert_eq!(p.to_nv12().uv, vec![1, 4, 2, 5, 3, 6, 128, 7]);
    }
}
