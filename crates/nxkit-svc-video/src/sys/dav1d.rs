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
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_flush,
    dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data,
};

use crate::av1::{Layout, Planar};

/// How often one temporal unit is offered to the decoder. A unit is taken at once, so this only stops a loop that
/// never ends.
const MAX_SENDS: usize = 64;

/// The most threads rav1d gets.
const MAX_THREADS: usize = 16;

/// The frame threads for `threads` worker threads: what dav1d picks for itself, `ceil(sqrt(threads))` and at most 8.
fn frame_delay(threads: usize) -> usize {
    let mut n = 1;
    while n * n < threads {
        n += 1;
    }
    n.clamp(1, 8)
}

/// A rav1d decoder.
///
/// Throughput. With one frame in flight rav1d decodes a picture with tile and row threads only, which keeps few cores
/// busy: a 1080p stream at 60 pictures a second fell behind on an M1 (the decoder threads ran at 7 to 15 percent).
/// Frame threading decodes `frame_delay` pictures at once, each on its own share of the threads, so every core works. The
/// price is latency: the picture of a unit comes out up to `frame_delay - 1` units later (3 on 8 cores, 4 on 16), 50 to
/// 67 ms at 60 pictures a second, which the caller covers with the timestamps that travel with every picture.
pub(crate) struct Dav1d {
    ctx: Option<Dav1dContext>,
    /// The frames that rav1d decodes at once. No more units than this wait inside it.
    delay: usize,
    /// Units sent whose picture is not out yet. At most `delay` of them are inside rav1d; a unit that shows nothing
    /// (a sequence header alone) would keep this above the real number, so [`Dav1d::in_flight`] caps it.
    sent: usize,
}

// SAFETY: a `Dav1dContext` is a counted reference to a decoder that dav1d documents as usable from any thread, one
// call at a time. Every call here takes `&mut self` or `&self` through one owner, and the context is closed in `drop`.
unsafe impl Send for Dav1d {}

impl Dav1d {
    /// Opens a decoder with frame threads: one worker thread per core up to 16, `ceil(sqrt(threads))` frames at once.
    pub(crate) fn open() -> Result<Dav1d, String> {
        let mut settings = std::mem::MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: the pointer is valid for a write of one `Dav1dSettings`, which the call fills.
        unsafe { dav1d_default_settings(NonNull::from(&mut settings).cast()) };
        // SAFETY: `dav1d_default_settings` wrote every field.
        let mut settings = unsafe { settings.assume_init() };
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(MAX_THREADS));
        let delay = frame_delay(threads);
        settings.n_threads = threads as c_int;
        settings.max_frame_delay = delay as c_int;
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
            (0, Some(ctx)) => Ok(Dav1d {
                ctx: Some(ctx),
                delay,
                sent: 0,
            }),
            (code, _) => Err(format!("dav1d could not be opened (code {code})")),
        }
    }

    /// The units that were sent and whose pictures are still inside the decoder: at most the frames it decodes at once.
    pub(crate) fn in_flight(&self) -> usize {
        self.sent.min(self.delay)
    }

    /// Sends one temporal unit with the timestamp `ts` and returns the pictures that are ready now, in display order,
    /// each with its own timestamp. With frame threads that is usually the picture of an earlier unit, and none at all
    /// for the first `frame_delay - 1` units. It never waits for a picture.
    ///
    /// rav1d drains by itself when `dav1d_get_picture` is called again with no data sent in between: that second call
    /// blocks until a frame in flight is done. So one call follows each send, and the pictures that the decoder holds
    /// back wait for the next send or for [`Dav1d::drain`].
    pub(crate) fn decode(&mut self, unit: &[u8], ts: i64) -> Result<Vec<Picture>, String> {
        let Some(ctx) = self.ctx else {
            return Err("the decoder is closed".to_string());
        };
        let mut out = Vec::new();
        if unit.is_empty() {
            return Ok(out);
        }
        let mut data = Dav1dData::default();
        // SAFETY: `data` is valid for a write. The call allocates `unit.len()` bytes for it.
        let buf = unsafe { dav1d_data_create(Some(NonNull::from(&mut data)), unit.len()) };
        let Some(buf) = NonNull::new(buf) else {
            return Err("dav1d has no memory for a packet".to_string());
        };
        // SAFETY: `buf` points at `unit.len()` writable bytes that nothing else uses.
        unsafe { std::ptr::copy_nonoverlapping(unit.as_ptr(), buf.as_ptr(), unit.len()) };
        data.m.timestamp = ts;

        let mut sends = 0;
        let mut taken = false;
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
            if data.sz == 0 {
                taken = true;
                self.sent += 1;
            }
            // One call: a second one would drain.
            if let Some(pic) = self.get()? {
                out.push(pic);
            }
        }
        if !taken {
            return Ok(out);
        }
        self.sent = self.sent.saturating_sub(out.len());
        Ok(out)
    }

    /// One `dav1d_get_picture`. `Ok(None)` for `EAGAIN`.
    fn get(&self) -> Result<Option<Picture>, String> {
        let Some(ctx) = self.ctx else {
            return Err("the decoder is closed".to_string());
        };
        let mut pic = Dav1dPicture::default();
        // SAFETY: `ctx` is open and `pic` is valid for a write.
        let r = unsafe { dav1d_get_picture(Some(ctx), Some(NonNull::from(&mut pic))) };
        match r.0 {
            0 => Ok(Some(Picture { pic })),
            code if would_block(code) => Ok(None),
            code => Err(format!("dav1d could not decode a picture (code {code})")),
        }
    }

    /// The end of the stream: every picture that is still inside the decoder, in display order. The first call after a
    /// send only arms the drain of rav1d (it answers `EAGAIN`), the next ones wait for the frames in flight.
    pub(crate) fn drain(&mut self) -> Result<Vec<Picture>, String> {
        let mut out = Vec::new();
        let mut calls = 0;
        loop {
            calls += 1;
            match self.get()? {
                Some(pic) => out.push(pic),
                None if calls == 1 => {}
                None => break,
            }
            if calls > MAX_THREADS * 4 {
                break;
            }
        }
        self.sent = 0;
        Ok(out)
    }

    /// Drops every picture inside the decoder and the state of the stream, for Stop, Flush and a seek. The next unit
    /// must start with a sequence header and a key frame.
    pub(crate) fn flush(&mut self) {
        if let Some(ctx) = self.ctx {
            // SAFETY: `ctx` is open.
            unsafe { dav1d_flush(ctx) };
        }
        self.sent = 0;
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
    /// The timestamp that the unit of this picture was sent with.
    pub(crate) fn timestamp(&self) -> i64 {
        self.pic.m.timestamp
    }

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
