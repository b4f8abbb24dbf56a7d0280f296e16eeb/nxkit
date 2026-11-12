//! The calls into Apple frameworks (VideoToolbox) and into the software AV1 decoder. macOS only.

#[cfg(target_os = "macos")]
pub(crate) mod dav1d;
pub(crate) mod vt;
