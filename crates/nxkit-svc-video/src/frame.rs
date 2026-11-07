//! One decoded picture.

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
    width: u32,
    height: u32,
    colorspace: u32,
    pub(crate) pixels: Pixels,
}

impl Frame {
    /// A picture from NV12 bytes in memory. `colorspace` is the `nv12-colorspace` of the SDK: 1 BT.601 limited,
    /// 2 BT.601 full, 3 BT.709 limited, 4 BT.709 full.
    pub fn from_nv12(width: u32, height: u32, colorspace: u32, planes: Nv12) -> Frame {
        Frame { width, height, colorspace, pixels: Pixels::Memory(planes) }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn from_buffer(width: u32, height: u32, colorspace: u32, buffer: crate::sys::vt::PixelBuffer) -> Frame {
        Frame { width, height, colorspace, pixels: Pixels::Buffer(buffer) }
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
            return Err(format!("the destination holds {} bytes, a {stride}x{slice} NV12 picture needs {need}", dst.len()));
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let mut copy = |y: &[u8], ys: usize, uv: &[u8], uvs: usize| {
            let (dy, duv) = dst.split_at_mut(stride * slice);
            copy_rows(y, ys, dy, stride, w.min(stride), h.min(slice));
            copy_rows(uv, uvs, duv, stride, (w.div_ceil(2) * 2).min(stride), h.div_ceil(2).min(slice / 2));
        };
        match &self.pixels {
            Pixels::Memory(m) => {
                copy(&m.y, m.y_stride, &m.uv, m.uv_stride);
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Pixels::Buffer(b) => b.with_planes(copy),
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
        Ok(Nv12 { y: all, y_stride: stride, uv, uv_stride: stride })
    }
}

/// Copies `rows` rows of `len` bytes from `src` to `dst`. A source that is too short for a row ends the copy.
fn copy_rows(src: &[u8], src_stride: usize, dst: &mut [u8], dst_stride: usize, len: usize, rows: usize) {
    for r in 0..rows {
        let s = r * src_stride;
        let d = r * dst_stride;
        let (Some(from), Some(to)) = (src.get(s..s + len.min(src_stride)), dst.get_mut(d..d + len)) else {
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
        Frame::from_nv12(w as u32, h as u32, 3, Nv12 { y, y_stride: w, uv, uv_stride: w })
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
    fn to_nv12_is_tight() {
        let f = picture(6, 4);
        let n = f.to_nv12().unwrap_or_else(|_| unreachable!());
        assert_eq!((n.y.len(), n.uv.len()), (24, 12));
        assert_eq!(n.y[..6], [0, 1, 2, 3, 4, 5]);
    }
}
