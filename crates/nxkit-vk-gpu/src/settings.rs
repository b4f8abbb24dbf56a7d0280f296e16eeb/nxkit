//! The settings of the Vulkan backend, as typed values from the host (R4). This crate reads no `NXKIT_*` variable.

use std::path::PathBuf;
use std::sync::Arc;

/// Called when the display showed a present: the present number (from 1, in the order of the game's presents) and the
/// time it was shown, in seconds on the `CACurrentMediaTime` clock.
pub type ShownHook = Arc<dyn Fn(u64, f64) + Send + Sync>;

/// What the host passes to [`crate::VkGpu::new`].
#[derive(Clone)]
pub struct VkGpuSettings {
    /// The folder of the translated shaders, `<run>/shaders`: it holds `vk_shaders.metallib` and `vk_shaders.tsv`
    /// (`nxkit_abi::vk_table::LIBRARY` and `FILE`).
    pub shader_dir: PathBuf,
    /// The extent of the surface that the game sees: 1920x1080 docked, 1280x720 handheld.
    pub surface_extent: (u32, u32),
    /// The minimum stay of the presents, which the host's refresh plan sets. None: plain presents.
    pub timed_present: Option<Arc<nxkit_metal::TimedPresent>>,
    /// Told when the display showed a present (the variable frame step, the screen state of the host).
    pub on_shown: Option<ShownHook>,
    /// How the present scales an image that is smaller than the drawable (R70).
    pub present_upscale: nxkit_metal::PresentUpscale,
}

impl Default for VkGpuSettings {
    fn default() -> Self {
        Self {
            shader_dir: PathBuf::from("shaders"),
            surface_extent: (1280, 720),
            timed_present: None,
            on_shown: None,
            present_upscale: nxkit_metal::PresentUpscale::Off,
        }
    }
}

impl std::fmt::Debug for VkGpuSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VkGpuSettings")
            .field("shader_dir", &self.shader_dir)
            .field("surface_extent", &self.surface_extent)
            .field("timed_present", &self.timed_present.is_some())
            .field("on_shown", &self.on_shown.is_some())
            .field("present_upscale", &self.present_upscale)
            .finish()
    }
}
