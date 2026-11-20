//! What the host tells the encoder, from the settings of the title.

use std::path::PathBuf;

use crate::GpuConfig;
use nxkit_metal::{ArchiveConfig, GobOrder};

/// The settings the encoder and the present path use.
#[derive(Clone, Debug)]
pub struct GpuSettings {
    /// The folder with the translated shaders (`<run>/shaders`).
    pub shader_dir: PathBuf,
    /// The Metal 4 pipeline archive to load, when the title uses one.
    pub archive: Option<PathBuf>,
    /// The folder of the archive shards (`<run>/pipelines`). Its index wins over `archive`, which Metal reads whole
    /// into memory.
    pub archive_shards: Option<PathBuf>,
    /// How many shards stay open at once. Zero picks the default of the Metal backend (32).
    pub open_shards: usize,
    /// Collect the pipelines that get built, so an archive can be written.
    pub collect_archive: bool,
    /// The threads that encode passes. Zero picks a number from the cores.
    pub threads: usize,
    /// The factor of the render size over the size the game asks for. 2.0 renders a 1080p game at 4K.
    pub scale: f32,
    /// Encode the frames on a thread of their own (`game.json` key `enc_thread`). The environment variable
    /// `NXKIT_ENC_THREAD=1` turns it on too.
    pub enc_thread: bool,
    /// The CPU runs one frame ahead of the GPU (`game.json` key `overlap`). `NXKIT_OVERLAP=1` turns it on too.
    pub overlap: bool,
    /// The order inside a GOB of the guest textures: [`GobOrder::Switch2`] for a game of the Switch 2.
    pub gob_order: GobOrder,
    /// A writable folder of the title for the runtime pipeline cache (see `pipelines/runtime.rs`), or none.
    pub runtime_cache: Option<PathBuf>,
    /// A number that changes with the app build: a runtime cache of another one is dropped.
    pub build_id: u64,
    /// The pipeline cache compiles the unspecialized pipeline of a base that the archive lacks and specializes it per
    /// colour state (`ArchiveConfig::compile_bases`): off unless `NXKIT_PIPELINE_BASES=1`. In Ocarina of Time every
    /// base was as rare as its pipelines (the vertex layout is part of it), an unspecialized compile took 0.1 to 10 s
    /// where the full compile took 1 to 6 ms, and text of the title screen went missing.
    pub compile_bases: bool,
    /// The pipeline cache specializes the unspecialized pipelines of the archive (`ArchiveConfig::exact_only` off):
    /// on unless `NXKIT_PIPELINE_SPECIALIZE=0`, which looks up and compiles exact pipelines only.
    pub specialize: bool,
    /// What the start shows while it makes the pipelines of the runtime cache (the host's loading screen).
    pub prewarm_screen: crate::ScreenHook,
}

/// [`GpuSettings::compile_bases`] from the settings of the host (`NXKIT_PIPELINE_BASES`).
pub fn pipeline_bases(env: &nxkit_abi::env::Env) -> bool {
    env.var("NXKIT_PIPELINE_BASES").is_ok_and(|v| v == "1")
}

/// [`GpuSettings::specialize`] from the settings of the host (`NXKIT_PIPELINE_SPECIALIZE`).
pub fn pipeline_specialize(env: &nxkit_abi::env::Env) -> bool {
    env.var("NXKIT_PIPELINE_SPECIALIZE")
        .map_or(true, |v| v != "0")
}

impl Default for GpuSettings {
    fn default() -> Self {
        Self {
            shader_dir: PathBuf::from("shaders"),
            archive: None,
            archive_shards: None,
            open_shards: 0,
            collect_archive: false,
            threads: 0,
            scale: 2.0,
            enc_thread: false,
            overlap: false,
            gob_order: GobOrder::Tegra,
            runtime_cache: None,
            build_id: 0,
            compile_bases: pipeline_bases(nxkit_abi::env::get()),
            specialize: pipeline_specialize(nxkit_abi::env::get()),
            prewarm_screen: crate::ScreenHook::default(),
        }
    }
}

impl GpuSettings {
    pub(crate) fn config(&self) -> GpuConfig {
        let defaults = GpuConfig::default();
        GpuConfig {
            shader_dir: self.shader_dir.clone(),
            threads: if self.threads > 0 {
                self.threads
            } else {
                defaults.threads
            },
            archive: ArchiveConfig {
                load: self.archive.clone(),
                shards: self.archive_shards.clone(),
                open_shards: self.open_shards,
                collect: self.collect_archive,
                compile_bases: self.compile_bases,
                exact_only: !self.specialize,
                runtime: Vec::new(),
            },
            gob_order: self.gob_order,
            runtime_cache: self.runtime_cache.clone(),
            build_id: self.build_id,
            prewarm_screen: self.prewarm_screen.clone(),
        }
    }
}
