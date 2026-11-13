//! One decoded picture.

use std::sync::atomic::{AtomicU64, Ordering};

/// The next picture number. Numbers start at 1, so 0 is never a picture.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// An NV12 picture in memory: a luma plane and one plane of interleaved Cb and Cr at half the size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nv12 {
    /// The luma plane, `y_stride` bytes a row.
    pub y: Vec<u8>,
    /// Bytes from one luma row to the next.
    pub y_stride: usize,
    /// The chroma plane, `uv_stride` bytes a row, `(height + 1) / 2` rows.
    pub uv: Vec<u8>,
    /// Bytes from one chroma row to the next.
    pub uv_stride: usize,
}

pub(crate) enum Pixels {
    Memory(Nv12),
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Buffer(crate::sys::vt::PixelBuffer),
}

/// A decoded picture, 8-bit 4:2:0 in the NV12 layout.
///
/// On Apple platforms a picture from the decoder stays in the `CVPixelBuffer` that VideoToolbox filled, in memory that Metal can
/// use. [`crate::texture::TextureCache`] turns it into textures without a copy (R20), and [`Frame::copy_nv12`] reads
/// it for a game that wants the bytes.
pub struct Frame {
    id: u64,
    width: u32,
    height: u32,
    colorspace: u32,
    pub(crate) pixels: Pixels,
}

impl Frame {
    /// A picture from NV12 bytes in memory. `colorspace` is the `nv12-colorspace` of the SDK: 1 BT.601 limited,
    /// 2 BT.601 full, 3 BT.709 limited, 4 BT.709 full.
    pub fn from_nv12(width: u32, height: u32, colorspace: u32, planes: Nv12) -> Frame {
        Frame {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            colorspace,
            pixels: Pixels::Memory(planes),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) fn from_buffer(
        width: u32,
        height: u32,
        colorspace: u32,
        buffer: crate::sys::vt::PixelBuffer,
    ) -> Frame {
        Frame {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            colorspace,
            pixels: Pixels::Buffer(buffer),
        }
    }

    /// The number of this picture: every decoded picture gets the next one, so it names the picture for as long as the
    /// process runs ([`crate::PictureSink`]).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The `nv12-colorspace` of the SDK: 1 BT.601 limited, 2 BT.601 full, 3 BT.709 limited, 4 BT.709 full.
    pub fn colorspace(&self) -> u32 {
        self.colorspace
    }

    /// Writes the picture into `dst` in the layout the SDK reports: the luma plane with `stride` bytes a row and
    /// `slice` rows, then the chroma plane with the same stride. `dst` must hold `stride * slice * 3 / 2` bytes.
    /// A picture that is smaller than `stride` by `slice` leaves the rest of the rows as they were.
    pub fn copy_nv12(&self, dst: &mut [u8], stride: usize, slice: usize) -> Result<(), String> {
        let need = stride * slice * 3 / 2;
        if dst.len() < need {
            return Err(format!(
                "the destination holds {} bytes, a {stride}x{slice} NV12 picture needs {need}",
                dst.len()
            ));
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let mut copy = |y: &[u8], ys: usize, uv: &[u8], uvs: usize| {
            let (dy, duv) = dst.split_at_mut(stride * slice);
            copy_rows(y, ys, dy, stride, w.min(stride), h.min(slice));
            copy_rows(
                uv,
                uvs,
                duv,
                stride,
                (w.div_ceil(2) * 2).min(stride),
                h.div_ceil(2).min(slice / 2),
            );
        };
        match &self.pixels {
            Pixels::Memory(m) => {
                copy(&m.y, m.y_stride, &m.uv, m.uv_stride);
                Ok(())
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Pixels::Buffer(b) => b.with_planes(copy),
        }
    }

    /// Calls `f` with the luma plane, its stride, the chroma plane and its stride, without a copy. A plane may be longer
    /// than `stride * rows` bytes or, for a decoder that cuts the last row short, shorter: read it with `get`.
    pub fn with_planes<R>(
        &self,
        f: impl FnOnce(&[u8], usize, &[u8], usize) -> R,
    ) -> Result<R, String> {
        match &self.pixels {
            Pixels::Memory(m) => Ok(f(&m.y, m.y_stride, &m.uv, m.uv_stride)),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Pixels::Buffer(b) => b.with_planes(f),
        }
    }

    /// The picture as bytes in memory, tightly packed (`width` bytes a row).
    pub fn to_nv12(&self) -> Result<Nv12, String> {
        let (w, h) = (self.width as usize, self.height as usize);
        let stride = w.div_ceil(2) * 2;
        let slice = h.div_ceil(2) * 2;
        let mut all = vec![0u8; stride * slice * 3 / 2];
        self.copy_nv12(&mut all, stride, slice)?;
        let uv = all.split_off(stride * slice);
        Ok(Nv12 {
            y: all,
            y_stride: stride,
            uv,
            uv_stride: stride,
        })
    }

    /// The picture as RGBA bytes, `width * 4` bytes a row and `height` rows, by the colour space of the picture. A
    /// picture smaller than `width` by `height` is padded with opaque black, a larger one is cropped.
    pub fn to_rgba(&self, width: u32, height: u32) -> Result<Vec<u8>, String> {
        let planes = self.to_nv12()?;
        let (w, h) = (width as usize, height as usize);
        let mut out = vec![0u8; w * h * 4];
        crate::yuv::nv12_to_rgba(
            &planes,
            self.width as usize,
            self.height as usize,
            self.colorspace,
            &mut out,
            w,
            h,
        );
        Ok(out)
    }
}

/// Copies `rows` rows of `len` bytes from `src` to `dst`. A source that is too short for a row ends the copy.
fn copy_rows(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    len: usize,
    rows: usize,
) {
    for r in 0..rows {
        let s = r * src_stride;
        let d = r * dst_stride;
        let (Some(from), Some(to)) = (src.get(s..s + len.min(src_stride)), dst.get_mut(d..d + len))
        else {
            return;
        };
        to[..from.len()].copy_from_slice(from);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: usize, h: usize) -> Frame {
        let y: Vec<u8> = (0..w * h).map(|i| (i % 251) as u8).collect();
        let uv: Vec<u8> = (0..w * h.div_ceil(2)).map(|i| (i % 241) as u8).collect();
        Frame::from_nv12(
            w as u32,
            h as u32,
            3,
            Nv12 {
                y,
                y_stride: w,
                uv,
                uv_stride: w,
            },
        )
    }

    #[test]
    fn copies_with_the_stride_the_sdk_asks_for() {
        let f = picture(4, 4);
        let mut dst = vec![0xEEu8; 8 * 4 * 3 / 2];
        assert!(f.copy_nv12(&mut dst, 8, 4).is_ok());
        assert_eq!(&dst[..4], &[0, 1, 2, 3]);
        assert_eq!(&dst[4..8], &[0xEE; 4]);
        assert_eq!(&dst[8..12], &[4, 5, 6, 7]);
        assert_eq!(&dst[8 * 4..8 * 4 + 4], &[0, 1, 2, 3]);
    }

    #[test]
    fn a_destination_that_is_too_small_is_an_error() {
        let f = picture(4, 4);
        assert!(f.copy_nv12(&mut [0u8; 10], 4, 4).is_err());
    }

    #[test]
    fn every_picture_has_its_own_number() {
        let (a, b) = (picture(2, 2), picture(2, 2));
        assert!(a.id() != 0 && b.id() > a.id());
    }

    #[test]
    fn with_planes_gives_the_planes_as_they_are() {
        let f = picture(6, 4);
        let r = f.with_planes(|y, ys, uv, uvs| (y.len(), ys, uv.len(), uvs));
        assert_eq!(r, Ok((24, 6, 12, 6)));
    }

    #[test]
    fn to_nv12_is_tight() {
        let f = picture(6, 4);
        let n = f.to_nv12().unwrap_or_else(|_| unreachable!());
        assert_eq!((n.y.len(), n.uv.len()), (24, 12));
        assert_eq!(n.y[..6], [0, 1, 2, 3, 4, 5]);
    }
}
