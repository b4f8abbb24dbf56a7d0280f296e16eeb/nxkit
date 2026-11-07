//! The platform layer: GameController, Core Haptics and the keyboard and mouse of the Mac.
//!
//! `Platform` has the same methods on every OS. Off macOS it has no controller, so the service builds and its
//! logic is tested there. Every `unsafe` block of the crate is in `apple`.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod apple;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod haptics;
#[cfg(target_os = "macos")]
pub(crate) use apple::Platform;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub(crate) use stub::Platform;
