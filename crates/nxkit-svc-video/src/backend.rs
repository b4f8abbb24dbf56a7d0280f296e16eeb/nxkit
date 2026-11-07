//! The stream decoder that the video service opens: VideoToolbox on macOS.
//!
//! VideoToolbox decodes VP9 on this Mac after `VTRegisterSupplementalVideoDecoderIfAvailable`. Its pictures stay in
//! `CVPixelBuffer`s that Metal can map (R20, R44). No library is vendored.

use crate::decoder::Backend;
use crate::vp9::KeyFrame;

#[cfg(target_os = "macos")]
mod apple {
    use std::sync::Arc;

    use crate::decoder::Backend;
    use crate::frame::Frame;
    use crate::sys::vt::Vp9Session;
    use crate::vp9::KeyFrame;

    pub(super) struct VideoToolbox {
        session: Vp9Session,
        width: u32,
        height: u32,
        colorspace: u32,
    }

    impl VideoToolbox {
        pub(super) fn open(key: &KeyFrame) -> Result<VideoToolbox, String> {
            Ok(VideoToolbox {
                session: Vp9Session::open(key)?,
                width: key.width,
                height: key.height,
                colorspace: key.nv12_colorspace(),
            })
        }
    }

    impl Backend for VideoToolbox {
        fn decode(&mut self, packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
            let picture = self.session.decode(packet)?;
            Ok(picture.map(|b| Arc::new(Frame::from_buffer(self.width, self.height, self.colorspace, b))))
        }
    }
}

/// Opens a VP9 decoder for a stream that starts with `key`.
#[cfg(target_os = "macos")]
pub(crate) fn open(key: &KeyFrame) -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(apple::VideoToolbox::open(key)?))
}

/// Opens a VP9 decoder for a stream that starts with `key`.
#[cfg(not(target_os = "macos"))]
pub(crate) fn open(_key: &KeyFrame) -> Result<Box<dyn Backend>, String> {
    Err("VP9 decoding uses VideoToolbox and is only available on macOS".to_string())
}
