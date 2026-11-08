//! The software side of the AV1 decoder: rav1d, the Rust port of dav1d, through its C interface. It decodes on a Mac
//! without an AV1 decoder in hardware, for a stream that VideoToolbox refuses, and for any stream when
//! `NXKIT_AV1_SOFTWARE=1`.

use std::ffi::c_int;
use std::ptr::NonNull;

use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422,
    DAV1D_PIXEL_LAYOUT_I444,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture,
    dav1d_open, dav1d_picture_unref, dav1d_send_data,
};

use crate::av1::{Layout, Planar};

/// How often one temporal unit is offered to the decoder. A unit is taken at once, so this only stops a loop that
/// never ends.
const MAX_SENDS: usize = 64;

/// A rav1d decoder.
pub(crate) struct Dav1d {
    ctx: Option<Dav1dContext>,
}

// SAFETY: a `Dav1dContext` is a counted reference to a decoder that dav1d documents as usable from any thread, one
// call at a time. Every call here takes `&mut self` or `&self` through one owner, and the context is closed in `drop`.
unsafe impl Send for Dav1d {}

impl Dav1d {
    /// Opens a decoder that gives one picture for each temporal unit before `decode` returns.
    pub(crate) fn open() -> Result<Dav1d, String> {
        let mut settings = std::mem::MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: the pointer is valid for a write of one `Dav1dSettings`, which the call fills.
        unsafe { dav1d_default_settings(NonNull::from(&mut settings).cast()) };
        // SAFETY: `dav1d_default_settings` wrote every field.
        let mut settings = unsafe { settings.assume_init() };
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(8));
        settings.n_threads = threads as c_int;
        // One frame at a time: the picture of a unit is out before the send returns.
        settings.max_frame_delay = 1;
        // Only the highest spatial layer.
        settings.all_layers = 0;

        let mut ctx: Option<Dav1dContext> = None;
        // SAFETY: both pointers are valid for the call. `ctx` receives a context that `drop` closes.
        let r = unsafe {
            dav1d_open(
                Some(NonNull::from(&mut ctx)),
                Some(NonNull::from(&mut settings)),
            )
        };
        match (r.0, ctx) {
            (0, Some(ctx)) => Ok(Dav1d { ctx: Some(ctx) }),
            (code, _) => Err(format!("dav1d could not be opened (code {code})")),
        }
    }

    /// Decodes one temporal unit. `Ok(None)` when it shows no picture. A unit shows at most one picture, and the last one
    /// wins if the decoder gave more.
    pub(crate) fn decode(&mut self, unit: &[u8]) -> Result<Option<Picture>, String> {
        let Some(ctx) = self.ctx else {
            return Err("the decoder is closed".to_string());
        };
        if unit.is_empty() {
            return Ok(None);
        }
        let mut data = Dav1dData::default();
        // SAFETY: `data` is valid for a write. The call allocates `unit.len()` bytes for it.
        let buf = unsafe { dav1d_data_create(Some(NonNull::from(&mut data)), unit.len()) };
        let Some(buf) = NonNull::new(buf) else {
            return Err("dav1d has no memory for a packet".to_string());
        };
        // SAFETY: `buf` points at `unit.len()` writable bytes that nothing else uses.
        unsafe { std::ptr::copy_nonoverlapping(unit.as_ptr(), buf.as_ptr(), unit.len()) };

        let mut shown: Option<Picture> = None;
        let mut sends = 0;
        while data.sz > 0 {
            sends += 1;
            if sends > MAX_SENDS {
                // SAFETY: `data` holds the unit that the decoder did not take.
                unsafe { dav1d_data_unref(Some(NonNull::from(&mut data))) };
                return Err("dav1d took no more data".to_string());
            }
            // SAFETY: `ctx` is open, `data` is a unit that `dav1d_data_create` made.
            let r = unsafe { dav1d_send_data(Some(ctx), Some(NonNull::from(&mut data))) };
            if r.0 != 0 && !would_block(r.0) {
                // SAFETY: as above. The decoder keeps the data when it refuses it.
                unsafe { dav1d_data_unref(Some(NonNull::from(&mut data))) };
                return Err(format!("dav1d refused a packet (code {})", r.0));
            }
            loop {
                let mut pic = Dav1dPicture::default();
                // SAFETY: `ctx` is open and `pic` is valid for a write.
                let r = unsafe { dav1d_get_picture(Some(ctx), Some(NonNull::from(&mut pic))) };
                if r.0 != 0 {
                    if !would_block(r.0) {
                        return Err(format!("dav1d could not decode a picture (code {})", r.0));
                    }
                    break;
                }
                shown = Some(Picture { pic });
            }
        }
        Ok(shown)
    }
}

/// Whether a negative result of dav1d is `EAGAIN`: the decoder wants the next call, nothing failed.
fn would_block(code: c_int) -> bool {
    std::io::Error::from_raw_os_error(-code).kind() == std::io::ErrorKind::WouldBlock
}

impl Drop for Dav1d {
    fn drop(&mut self) {
        let mut ctx = self.ctx.take();
        // SAFETY: `ctx` is an open context and nothing uses it after this. The call frees it and clears `ctx`.
        unsafe { dav1d_close(Some(NonNull::from(&mut ctx))) };
    }
}

/// A picture of the decoder. It gives back its memory when it drops.
pub(crate) struct Picture {
    pic: Dav1dPicture,
}

// SAFETY: the picture owns a counted reference to its planes, which nothing writes after the decoder has output it.
unsafe impl Send for Picture {}

impl Picture {
    /// The `matrix_coefficients` of the sequence header (as in `av1::Sequence::matrix`), and whether the range is full.
    pub(crate) fn matrix_and_range(&self) -> (u8, bool) {
        match self.pic.seq_hdr {
            // SAFETY: the sequence header lives as long as the picture, which holds a reference to it.
            Some(h) => unsafe { (h.as_ref().mtrx as u8, h.as_ref().color_range != 0) },
            None => (2, false),
        }
    }

    /// Runs `f` with the planes of the picture.
    pub(crate) fn with_planar<R>(&self, f: impl FnOnce(&Planar<'_>) -> R) -> Option<R> {
        let (w, h) = (
            usize::try_from(self.pic.p.w).ok()?,
            usize::try_from(self.pic.p.h).ok()?,
        );
        let bit_depth = u8::try_from(self.pic.p.bpc).ok()?;
        let layout = match self.pic.p.layout {
            DAV1D_PIXEL_LAYOUT_I400 => Layout::Mono,
            DAV1D_PIXEL_LAYOUT_I420 => Layout::I420,
            DAV1D_PIXEL_LAYOUT_I422 => Layout::I422,
            DAV1D_PIXEL_LAYOUT_I444 => Layout::I444,
            _ => return None,
        };
        let sample = if bit_depth > 8 { 2 } else { 1 };
        let (cw, ch) = match layout {
            Layout::Mono => (0, 0),
            Layout::I420 => (w.div_ceil(2), h.div_ceil(2)),
            Layout::I422 => (w.div_ceil(2), h),
            Layout::I444 => (w, h),
        };
        let plane = |i: usize, width: usize, rows: usize| -> Option<(&[u8], usize)> {
            if rows == 0 || width == 0 {
                return Some((&[], 0));
            }
            let ptr = self.pic.data[i]?;
            let stride = usize::try_from(self.pic.stride[usize::from(i > 0)])
                .ok()
                .filter(|s| *s > 0)?;
            let len = stride * (rows - 1) + width * sample;
            // SAFETY: the decoder allocated this plane with `rows` rows of `stride` bytes, the last of them at least
            // `width * sample` bytes long, and it stays alive and unchanged while `self` holds the picture.
            Some((
                unsafe { std::slice::from_raw_parts(ptr.as_ptr().cast::<u8>(), len) },
                stride,
            ))
        };
        let planes = [plane(0, w, h)?, plane(1, cw, ch)?, plane(2, cw, ch)?];
        Some(f(&Planar {
            width: w,
            height: h,
            bit_depth,
            layout,
            planes,
        }))
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: the picture came from `dav1d_get_picture` and is released once.
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut self.pic))) };
    }
}
