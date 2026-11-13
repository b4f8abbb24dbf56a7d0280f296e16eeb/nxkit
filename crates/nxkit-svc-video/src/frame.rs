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
    #[cfg(target_os = "macos")]
    Buffer(crate::sys::vt::PixelBuffer),
}

/// A decoded picture, 8-bit 4:2:0 in the NV12 layout.
///
/// On macOS a picture from the decoder stays in the `CVPixelBuffer` that VideoToolbox filled, in memory that Metal can
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

    #[cfg(target_os = "macos")]
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

    /// Calls `f` with the luma plane, its stride, the chroma plane and its stride, without a copy. A plane may be longer
    /// than `stride * rows` bytes or, for a decoder that cuts the last row short, shorter: read it with `get`.
    pub fn with_planes<R>(
        &self,
        f: impl FnOnce(&[u8], usize, &[u8], usize) -> R,
    ) -> Result<R, String> {
        match &self.pixels {
            Pixels::Memory(m) => Ok(f(&m.y, m.y_stride, &m.uv, m.uv_stride)),
            #[cfg(target_os = "macos")]
            Pixels::Buffer(b) => b.with_planes(f),
        }
    }

    /// Copies the picture to the SDK stride and slice layout with checked sizes.
    pub fn copy_nv12(&self, dst: &mut [u8], stride: usize, slice: usize) -> Result<(), String> {
        let luma = stride.checked_mul(slice).ok_or("NV12 size overflow")?;
        let chroma = stride.checked_mul(slice / 2).ok_or("NV12 size overflow")?;
        let length = luma.checked_add(chroma).ok_or("NV12 size overflow")?;
        let target = dst
            .get_mut(..length)
            .ok_or("NV12 destination is too small")?;
        let width = self.width() as usize;
        let height = self.height() as usize;
        self.with_planes(|y, ys, uv, uvs| {
            let (dy, duv) = target.split_at_mut(luma);
            rows(y, ys, dy, stride, width.min(stride), height.min(slice))?;
            rows(
                uv,
                uvs,
                duv,
                stride,
                (width.div_ceil(2) * 2).min(stride),
                height.div_ceil(2).min(slice / 2),
            )
        })?
    }
    /// Copies the picture into tightly packed, even-sized NV12 planes.
    pub fn to_nv12(&self) -> Result<Nv12, String> {
        let stride = (self.width() as usize).div_ceil(2) * 2;
        let slice = (self.height() as usize).div_ceil(2) * 2;
        let luma = stride.checked_mul(slice).ok_or("NV12 size overflow")?;
        let length = luma.checked_add(luma / 2).ok_or("NV12 size overflow")?;
        let mut data = Vec::new();
        data.try_reserve_exact(length)
            .map_err(|e| format!("NV12 allocation failed: {e}"))?;
        data.resize(length, 0);
        self.copy_nv12(&mut data, stride, slice)?;
        let uv = data.split_off(luma);
        Ok(Nv12 {
            y: data,
            y_stride: stride,
            uv,
            uv_stride: stride,
        })
    }
    /// Converts the picture to RGBA, padding or cropping to the requested size.
    pub fn to_rgba(&self, width: u32, height: u32) -> Result<Vec<u8>, String> {
        let planes = self.to_nv12()?;
        let length = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or("RGBA size overflow")?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(length)
            .map_err(|e| format!("RGBA allocation failed: {e}"))?;
        rgba.resize(length, 0);
        crate::yuv::nv12_to_rgba(
            &planes,
            self.width() as usize,
            self.height() as usize,
            self.colorspace,
            &mut rgba,
            width as usize,
            height as usize,
        );
        Ok(rgba)
    }
}

fn rows(
    src: &[u8],
    ss: usize,
    dst: &mut [u8],
    ds: usize,
    width: usize,
    count: usize,
) -> Result<(), String> {
    for row in 0..count {
        let start = row.checked_mul(ss).ok_or("NV12 row overflow")?;
        let end = start
            .checked_add(width.min(ss))
            .ok_or("NV12 row overflow")?;
        let input = src
            .get(start..end)
            .ok_or("NV12 source plane is truncated")?;
        let start = row.checked_mul(ds).ok_or("NV12 row overflow")?;
        let end = start.checked_add(input.len()).ok_or("NV12 row overflow")?;
        dst.get_mut(start..end)
            .ok_or("NV12 destination plane is truncated")?
            .copy_from_slice(input);
    }
    Ok(())
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
    #[test]
    fn overflow_and_truncated_planes_are_errors() {
        assert!(picture(2, 2).copy_nv12(&mut [], usize::MAX, 4).is_err());
        let truncated = Frame::from_nv12(
            2,
            2,
            3,
            Nv12 {
                y: vec![1],
                y_stride: 2,
                uv: vec![1, 2],
                uv_stride: 2,
            },
        );
        assert!(truncated.to_nv12().is_err());
    }
}
