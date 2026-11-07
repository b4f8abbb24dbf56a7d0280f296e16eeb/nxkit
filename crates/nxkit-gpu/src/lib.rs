//! The frame encoder of the nxkit host.
//!
//! This crate turns a held NVN frame (`nxkit_nvn::frame::Frame` and the passes of `nxkit_nvn::passes::PassPlan`) into
//! Metal 4 work through `nxkit-metal`. It is the layer between the NVN front end and the GPU. Refer to
//! `docs/nxkit-gpu.md` for the design and its limits.
//!
//! - [`Gpu`] is the entry point. [`Gpu::encode_frame`] encodes and submits one frame.
//! - [`detail`] holds the side channel that carries what the draws bound (programs, fixed function state, buffers,
//!   texture handles). The recorder of the front end fills it.
//! - [`stats`] holds the counters of a frame and the report that the caller applies to its resource table.

#![cfg(target_os = "macos")]

mod backend;
mod barriers;
mod blit;
mod bindless;
pub mod detail;
mod draws;
mod encode;
mod format_map;
mod guest;
mod gpu;
mod icbs;
mod jobs;
mod json;
mod passes;
mod pipelines;
mod programs;
mod settings;
pub mod stats;
mod textures;
mod translate;
mod upload;
mod walk;

pub use backend::{GpuBackend, MAX_PENDING};
pub use gpu::{Gpu, GpuConfig};
pub use settings::GpuSettings;
pub use stats::{EncodeReport, EncodeStats, Skip};
