//! nn::ndve's movie network, run natively.
//!
//! Switch 2 games that use the SDK's movie library upscale each movie picture with a small neural network
//! (`Movie/Ndve/*.dat`, a U-Net of 12 convolution layers and a resampler). The game runs it as a chain of compute
//! dispatches with Ampere tensor code. This crate holds what it takes to run the same network with kernels written for
//! Apple GPUs instead, and to check that a game runs exactly this network. Refer to `docs/plans/ndve-native.md` for the
//! network and `docs/plans/ndve-impl.md` for the plan.
//!
//! The crate is pure Rust and uses only `std`. It does no I/O and has no Metal code: `nxkit-gpu` reads the game's
//! file, asks [`file::Network::parse`] and [`fingerprint`], and encodes the [`plan::Plan`] with the MSL of [`msl`].
//!
//! - [`ane`]: the network as a Core ML ML Program for the Neural Engine (the files of an `.mlpackage`).
//! - [`mod@file`]: the `.dat` container, its networks and the weight blobs.
//! - [`fingerprint`]: the checks that a file and a dispatch sequence are the network this crate implements.
//! - [`net`]: the layer table of the network (shapes, offsets, kinds).
//! - [`pack`]: the weights reordered into the layout the kernels read.
//! - [`plan`]: the native passes as data: buffers, kernels, grids and bindings.
//! - [`msl`]: the Metal kernels.
//! - [`mod@reference`]: a CPU implementation with the rounding of the hardware, for tests and diagnostics.
//! - [`half`]: software f16 with round to nearest even, for the reference.

pub mod ane;
pub mod file;
pub mod fingerprint;
pub mod half;
pub mod kernels;
pub mod msl;
pub mod net;
pub mod pack;
pub mod plan;
pub mod reference;
