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
//! - `diag` holds the opt-in diagnostics (`NXKIT_DIAG`, `NXKIT_KNOB`), see `docs/diagnostics.md`. The render health
//!   report lives in `nxkit_nvn::diag`.

#![cfg(target_os = "macos")]

mod diag;
pub(crate) use diag::{diag, knob, nobar, nobar_range, refresh_knobs, watched};
pub(crate) use skip::{buf_skip, tex_skip};

mod backend;
mod barr;
mod barriers;
mod bindless;
mod blit;
mod clear;
mod copyback;
mod coverage;
pub mod detail;
mod drawctx;
mod draws;
mod encode;
mod encthread;
mod fastmap;
mod format_map;
mod gatediag;
mod geometry;
mod gmem;
mod gpu;
mod guest;
mod guestpools;
mod health;
mod icbs;
mod images;
mod indirect;
mod jobs;
mod json;
mod lru;
mod lum;
mod mesh;
mod meshvis;
mod nanscan;
mod native;
mod ordinal;
mod overlap;
mod passes;
mod passprof;
mod pipelines;
mod poolreg;
mod pools;
mod private;
mod programs;
mod pull;
mod queries;
mod rename;
mod report;
mod scaled;
mod settings;
mod skip;
pub mod snapshot;
pub mod stats;
mod textures;
mod translate;
mod upload;
mod walk;

pub use backend::{GpuBackend, MAX_PENDING};
pub use gpu::{Gpu, GpuConfig};
pub use guestpools::GuestPools;
pub use pipelines::{PrewarmScreen, flush_runtime_cache, set_prewarm_screen};
pub use pools::{
    Backing, BadRange, Drained, Loc, MakeBacking, MetalPoolSet, Miss, PAGE, PagePins, PoolBuffer,
    PoolEntry, PoolSet, PoolSnapshot, PoolStats,
};
pub use report::EncodeReport;
pub use settings::GpuSettings;
pub use skip::Skip;
pub use snapshot::{EncodeView, Live, Snapshot};
pub use stats::{DrawClass, EncodeStats, KindStats};
pub use textures::inventory;
