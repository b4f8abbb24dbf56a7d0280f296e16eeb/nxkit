//! The frame encoder of the nxkit host.
//!
//! This crate turns a held NVN frame (`nxkit_nvn::frame::Frame` and the passes of `nxkit_nvn::passes::PassPlan`) into
//! Metal 4 work through `nxkit-metal`. It is the layer between the NVN front end and the GPU. Refer to
//! `docs/nxkit-gpu.md` for the design and its limits.
//!
//! - [`Gpu`] is the entry point. [`Gpu::encode_frame`] encodes and submits one frame.
//! - [`detail`] holds the side channel that carries what the draws bound (programs, fixed function state, buffers,
//!   texture handles). The recorder of the front end fills it.
//! - [`stats`] holds the counters of a frame, [`EncodeReport`] what the caller applies to its resource table.
//! - `diag` holds the switches of the opt-in diagnostics (`NXKIT_DIAG`, `NXKIT_KNOB`), see `docs/diagnostics.md`. The
//!   analyses (`nanscan`, `lum`, `passprof`, `meshvis` and others) see the encode path through `probe` only. The render
//!   health report lives in `nxkit_nvn::diag`.
//!
//! The modules form layers (W29, `docs/architecture.md`): support (`fastmap`, `ordinal`, `skip`, `diag`, ...), frame
//! data (`detail`, `stats`, `pools`, ...), resources (`textures`, `pipelines`, `bindless`, ...), planning (`drawctx`,
//! `draws`, `probe`, ...), encode (`encode`, `passes`), the analyses, and the frame driver (`gpu`, `backend`, `walk`).

#![cfg(target_os = "macos")]

mod diag;
pub(crate) use diag::{diag, knob, nobar, nobar_range, refresh_knobs, watched};
pub(crate) use skip::{buf_skip, tex_skip};

mod backend;
mod barriers;
mod bindless;
mod blit;
mod clear;
mod copyback;
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
mod prewarm_screen;
mod private;
mod probe;
mod programs;
mod pull;
mod queries;
mod rename;
mod report;
mod scaled;
mod settings;
mod shown;
mod skip;
pub mod snapshot;
pub mod stats;
mod textures;
mod translate;
mod ubotex;
mod upload;
mod walk;

pub use backend::{GpuBackend, MAX_PENDING};
pub use gpu::{Gpu, GpuConfig};
pub use guestpools::GuestPools;
pub use pipelines::flush_runtime_cache;
pub use pools::{
    Backing, BadRange, Drained, Loc, MakeBacking, MetalPoolSet, Miss, PAGE, PagePins, PoolBuffer,
    PoolEntry, PoolSet, PoolSnapshot, PoolStats,
};
pub use prewarm_screen::{PrewarmScreen, ScreenHook};
pub use report::EncodeReport;
pub use settings::{
    GpuEnv, GpuSettings, NativeNet, install as install_env, pipeline_bases, pipeline_specialize,
};
pub use shown::when_shown;
pub use skip::Skip;
pub use snapshot::{EncodeView, Live, Snapshot};
pub use stats::{DrawClass, EncodeStats, KindStats};
pub use textures::inventory;
