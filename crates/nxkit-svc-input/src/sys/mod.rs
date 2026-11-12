//! The platform layer: GameController, Core Haptics and the keyboard and mouse of the Mac.
//!
//! `Platform` has the same methods on every OS. Off macOS it has no controller, so the service builds and its
//! logic is tested there. Every `unsafe` block of the crate is in `apple`.

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
mod apple;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
mod haptics;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) use apple::Platform;

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
mod stub;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) use stub::Platform;

#[cfg(any(target_os = "ios", test))]
pub(crate) mod virtual_pad;
