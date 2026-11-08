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

/// Whether `NXKIT_DIAG` names `cat` or `all`. The value is a comma separated list of categories.
pub(crate) fn diag(cat: &str) -> bool {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| std::env::var("NXKIT_DIAG").unwrap_or_default())
        .split(',')
        .any(|c| c == cat || c == "all")
}

/// Whether `NXKIT_KNOB` names `name`. Knobs change what the encoder does, for experiments while debugging a game:
/// `nodepth` turns the depth and stencil tests off, `nocull` turns culling off, `noeq` turns the depth test
/// `equal` into `always`.
pub(crate) fn knob(name: &str) -> bool {
    KNOBS.get_or_init(Knobs::new).flags.load(std::sync::atomic::Ordering::Relaxed) & knob_bit(name) != 0
}

struct Knobs {
    flags: std::sync::atomic::AtomicU32,
    file: Option<std::path::PathBuf>,
}
static KNOBS: std::sync::OnceLock<Knobs> = std::sync::OnceLock::new();

fn knob_bit(name: &str) -> u32 {
    match name { "nodepth" => 1, "nocull" => 2, "nooffset" => 4, "flat" => 8, "noeq" => 16, "paint" => 32, "cubearr" => 64, "paintdir" => 128, "cubesrc" => 256, "cubesrcs" => 512, "allbar" => 1024, "hmpaint" => 2048, _ => 0 }
}

impl Knobs {
    fn new() -> Self {
        let value = std::env::var("NXKIT_KNOB").unwrap_or_default();
        Self { flags: std::sync::atomic::AtomicU32::new(value.split(',').map(|s| knob_bit(s.trim())).fold(0, |a, b| a | b)),
            file: std::env::var_os("NXKIT_KNOB_FILE").map(std::path::PathBuf::from) }
    }
}

/// The pass range `[lo, hi)` that the `/tmp/fmap/nobar` file gives a full barrier each, for bisecting barrier problems.
static NOBAR: std::sync::Mutex<Option<(usize, usize)>> = std::sync::Mutex::new(None);

/// True when pass `index` gets a full barrier from the `/tmp/fmap/nobar` file ("lo hi").
pub(crate) fn nobar_range() -> Option<(usize, usize)> {
    NOBAR.lock().ok().and_then(|r| *r)
}

/// True when pass `index` lies in [`nobar_range`].
pub(crate) fn nobar(index: usize) -> bool {
    NOBAR.lock().ok().and_then(|r| *r).is_some_and(|(lo, hi)| (lo..hi).contains(&index))
}

/// Reloads opt-in diagnostic switches once per frame, so experiments do not need another game boot.
pub(crate) fn refresh_knobs() {
    if let Ok(mut r) = NOBAR.lock() {
        *r = std::fs::read_to_string("/tmp/fmap/nobar").ok().and_then(|t| {
            let mut it = t.split_whitespace().filter_map(|w| w.parse::<usize>().ok());
            Some((it.next()?, it.next()?))
        });
    }
    let k = KNOBS.get_or_init(Knobs::new);
    if let Some(file) = &k.file {
        if let Ok(value) = std::fs::read_to_string(file) {
            let flags = value.split(',').map(|s| knob_bit(s.trim())).fold(0, |a, b| a | b);
            k.flags.store(flags, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Prints a rate-limited diagnostic line of category `tex` and gives back `Skip::Texture`.
pub(crate) fn tex_skip(what: std::fmt::Arguments<'_>) -> stats::Skip {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if diag("tex") && SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 300 {
        eprintln!("DIAG tex {what}");
    }
    stats::Skip::Texture
}

pub(crate) fn buf_skip(what: std::fmt::Arguments<'_>) -> stats::Skip {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if diag("buf") && SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 300 {
        eprintln!("DIAG buf {what}");
    }
    stats::Skip::Buffer
}

mod backend;
mod barr;
mod capture;
mod clear;
mod copyback;
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
mod lum;
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
