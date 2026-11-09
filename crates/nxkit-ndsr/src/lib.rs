//! agl's NDSR upscaler, run natively.
//!
//! Switch 2 games that use the SDK's NDSR library (agl in Breath of the Wild's Switch 2 Edition, `nn::ndve`'s sibling)
//! upscale the picture with a small neural network chosen by the dynamic resolution. This crate holds what it takes to
//! run the same network with kernels written for Apple GPUs, and to check that a game runs exactly this network.
//! Refer to `docs/plans/ndsr-native.md` for the network and `docs/plans/ndsr-impl.md` for the plan.
//!
//! The crate is pure Rust and uses only `std` (and `nxb-model` for SHA-256). It does no I/O and has no Metal code:
//! `nxkit-gpu` finds the network with [`fingerprint`] and encodes the [`plan::Plan`] of [`kernels`].
//!
//! - [`plan`]: the native passes as data, shared with `nxkit-ndve` (`nxkit-native-plan`).
//! - [`net`], [`fingerprint`], [`pack_file`], [`reference`]: package A, the table, the checks, the pack reader, the CPU
//!   reference.
//! - [`pack`], [`msl`], [`kernels`]: package B, the weight layout, the Metal source and the plan.

pub mod fingerprint;
pub mod kernels;
pub mod msl;
pub mod net;
pub mod pack;
pub mod pack_file;
pub mod plan {
    //! The native passes as data, shared with `nxkit-ndve`.
    pub use nxkit_native_plan::*;
}
pub mod reference;
