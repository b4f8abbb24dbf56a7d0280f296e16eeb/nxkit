//! iOS codec selection. Unsupported hardware and formats fail explicitly.
use crate::{decoder::Backend, vp9::KeyFrame};
#[cfg(target_os = "ios")]
mod device {
    use crate::{
        av1::{Sequence, find_sequence},
        decoder::Backend,
        frame::Frame,
        sys::vt::{self, Session},
        vp9::KeyFrame,
    };
    use std::sync::Arc;
    pub(super) struct Vp9 {
        session: Session,
        colorspace: u32,
    }
    impl Vp9 {
        pub(super) fn open(key: &KeyFrame) -> Result<Self, String> {
            Ok(Self {
                session: Session::vp9(key)?,
                colorspace: key.nv12_colorspace(),
            })
        }
    }
    impl Backend for Vp9 {
        fn decode(&mut self, packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
            Ok(self.session.decode(packet)?.map(|buffer| {
                let (width, height) = buffer.size();
                Arc::new(Frame::from_buffer(width, height, self.colorspace, buffer))
            }))
        }
    }
    pub(super) struct Av1 {
        sequence: Option<Sequence>,
        session: Option<Session>,
    }
    impl Av1 {
        pub(super) fn open() -> Result<Self, String> {
            if !vt::av1_hardware() {
                return Err(
                    "This iOS device has no VideoToolbox AV1 decoder; software AV1 is not included"
                        .into(),
                );
            }
            Ok(Self {
                sequence: None,
                session: None,
            })
        }
    }
    impl Backend for Av1 {
        fn decode(&mut self, packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
            if let Some(sequence) =
                find_sequence(packet).filter(|s| self.sequence.as_ref() != Some(s))
            {
                // Invalidate the old session before a changed, possibly unsupported sequence.
                self.session = None;
                self.sequence = None;
                self.session = Some(Session::av1(&sequence)?);
                self.sequence = Some(sequence);
            }
            let (Some(sequence), Some(session)) = (&self.sequence, &mut self.session) else {
                return Err("AV1 packet has no usable sequence header".into());
            };
            Ok(session.decode(packet)?.map(|buffer| {
                let (width, height) = buffer.size();
                Arc::new(Frame::from_buffer(
                    width,
                    height,
                    sequence.nv12_colorspace(height),
                    buffer,
                ))
            }))
        }
    }
}
#[cfg(target_os = "ios")]
pub(crate) fn open(key: &KeyFrame) -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(device::Vp9::open(key)?))
}
#[cfg(target_os = "ios")]
pub(crate) fn open_av1() -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(device::Av1::open()?))
}
#[cfg(not(target_os = "ios"))]
pub(crate) fn open(_: &KeyFrame) -> Result<Box<dyn Backend>, String> {
    Err("The iOS VideoToolbox backend requires an iOS device".into())
}
#[cfg(not(target_os = "ios"))]
pub(crate) fn open_av1() -> Result<Box<dyn Backend>, String> {
    Err("The iOS VideoToolbox backend requires an iOS device".into())
}
#[cfg(all(test, not(target_os = "ios")))]
mod tests {
    #[test]
    fn no_fake_host_decoder() {
        assert!(super::open_av1().is_err());
    }
}
