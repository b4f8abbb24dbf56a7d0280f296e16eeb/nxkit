//! Core ML on the Neural Engine, without copies.
//!
//! This crate is plumbing for a Core ML ML Program (an `.mlpackage`). It knows nothing about the network inside.
//!
//! - [`Model::compile_and_load`]: compiles the package once, keeps the `.mlmodelc` in a cache directory under a key, and
//!   loads it for the CPU and the Neural Engine (never the GPU). It blocks, so call it on a background thread.
//! - [`Model::compute_plan`], [`PlanReport`]: which device each operation of the program prefers. A caller that wants
//!   the Neural Engine only refuses a model with [`PlanReport::all_on_neural_engine`] false.
//! - [`SharedTensor`]: an f16 tensor on an IOSurface (a `CVPixelBuffer` of `OneComponent16Half`; the channels are
//!   stacked as rows). Core ML reads inputs and writes outputs there, and `nxkit-metal` wraps the same pages as a
//!   texture, so a picture goes from the GPU to the Neural Engine and back without a copy.
//! - [`Model::predict_async`]: one prediction with several inputs and several outputs. The outputs are
//!   `outputBackings`, so Core ML writes into the shared tensors. A completion callback tells when they are filled.
//!
//! The crate builds on macOS only. On other systems it is empty, so the workspace still builds there.

#![cfg(target_os = "macos")]

mod cache;
mod error;
mod model;
mod plan;
mod predict;
mod tensor;

pub use cache::os_build_tag;
pub use error::{CoreMlError, Result};
pub use model::{Model, TensorInfo};
pub use plan::{ComputeDevice, PlanReport};
pub use tensor::SharedTensor;
// The surface type of `SharedTensor::io_surface`, so a caller needs no dependency of its own to name it.
pub use objc2_io_surface::IOSurfaceRef;
