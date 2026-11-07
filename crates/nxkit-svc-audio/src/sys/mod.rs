//! The only module of the crate that uses `unsafe`: the macOS FFI, the lock-free queue and calls into guest code.
//! The items are visible inside the crate only.

#![allow(unsafe_code, unreachable_pub)]

pub(crate) mod au;
pub(crate) mod call;
pub(crate) mod mem;
pub(crate) mod ring;
pub(crate) mod sem;
pub(crate) mod vdsp;
