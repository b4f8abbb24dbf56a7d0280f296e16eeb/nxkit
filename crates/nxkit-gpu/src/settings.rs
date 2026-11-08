//! What the host tells the encoder, from the settings of the title.

use std::path::PathBuf;

use crate::GpuConfig;
use nxkit_metal::ArchiveConfig;

/// The settings the encoder and the present path use.
#[derive(Clone, Debug)]
pub struct GpuSettings {
    /// The folder with the translated shaders (`<run>/shaders`).
    pub shader_dir: PathBuf,
    /// The Metal 4 pipeline archive to load, when the title uses one.
    pub archive: Option<PathBuf>,
    /// Collect the pipelines that get built, so an archive can be written.
    pub collect_archive: bool,
    /// The threads that encode passes. Zero picks a number from the cores.
    pub threads: usize,
    /// The factor of the render size over the size the game asks for. 2.0 renders a 1080p game at 4K.
    pub scale: f32,
    /// Encode the frames on a thread of their own (`game.json` key `enc_thread`). The environment variable
    /// `NXKIT_ENC_THREAD=1` turns it on too.
    pub enc_thread: bool,
}

impl Default for GpuSettings {
    fn default() -> Self {
        Self {
            shader_dir: PathBuf::from("shaders"),
            archive: None,
            collect_archive: false,
            threads: 0,
            scale: 2.0,
            enc_thread: false,
        }
    }
}

impl GpuSettings {
    pub(crate) fn config(&self) -> GpuConfig {
        let defaults = GpuConfig::default();
        GpuConfig {
            shader_dir: self.shader_dir.clone(),
            threads: if self.threads > 0 { self.threads } else { defaults.threads },
            archive: ArchiveConfig {
                load: self.archive.clone(),
                collect: self.collect_archive,
            },
            ..defaults
        }
    }
}
