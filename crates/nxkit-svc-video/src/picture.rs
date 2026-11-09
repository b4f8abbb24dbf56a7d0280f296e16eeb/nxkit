//! The hand-off of decoded pictures to a consumer that works ahead of the game (the Neural Engine path of the GPU
//! front end): it learns of a picture as soon as the decoder has it, and later which guest memory the game read it into.

use std::sync::{Arc, OnceLock};

use crate::frame::Frame;

/// Who is told about pictures. Both calls come from threads of the service (the decoder's worker and the game's
/// thread) and must return quickly.
pub trait PictureSink: Send + Sync {
    /// A picture was decoded, up to the decoder's lookahead (three pictures) ahead of the game. Called with the lock of the
    /// decoder held: take a clone of `frame` and return.
    fn decoded(&self, frame: &Arc<Frame>);

    /// The game read picture `frame` (a [`Frame::id`]) as NV12 into guest memory: the luma plane at the host address
    /// `luma` and the chroma plane at `chroma`, `width` by `height` pixels, tightly packed.
    fn read_nv12(&self, frame: u64, luma: u64, chroma: u64, width: u32, height: u32);
}

static SINK: OnceLock<Arc<dyn PictureSink>> = OnceLock::new();

/// Sets the sink. The first call counts, later ones are ignored.
pub fn set_picture_sink(sink: Arc<dyn PictureSink>) {
    let _ = SINK.set(sink);
}

pub(crate) fn decoded(frame: &Arc<Frame>) {
    if let Some(s) = SINK.get() {
        s.decoded(frame);
    }
}

pub(crate) fn read_nv12(frame: u64, luma: u64, chroma: u64, width: u32, height: u32) {
    if let Some(s) = SINK.get() {
        s.read_nv12(frame, luma, chroma, width, height);
    }
}
