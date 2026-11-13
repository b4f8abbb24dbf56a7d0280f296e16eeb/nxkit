//! The only module of the crate that uses `unsafe`: the macOS FFI, the lock-free queue and calls into guest code.
//! The items are visible inside the crate only.

#![allow(unsafe_code, unreachable_pub)]

#[cfg(not(target_os = "ios"))]
pub(crate) mod au;
#[cfg(target_os = "ios")]
#[path = "remoteio.rs"]
pub(crate) mod au;
pub(crate) mod call;
pub(crate) mod mem;
#[cfg(all(test, not(target_os = "ios")))]
mod remoteio;
pub(crate) mod ring;
pub(crate) mod sem;
pub(crate) mod vdsp;
