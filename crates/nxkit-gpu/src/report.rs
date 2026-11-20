//! What one encoded frame gives back to the caller. It carries the counters of [`stats`](crate::stats) and the GPU
//! counters of the frame (`crate::queries`), so it sits above both (W29).

use nxkit_nvn::resource::TexId;

use crate::stats::EncodeStats;

/// The result of `Gpu::encode_frame`. The `nxkit-nvn` front end owns the resource table, so the caller applies it.
#[derive(Clone, Debug, Default)]
pub struct EncodeReport {
    /// The number of the submitted batch, or 0 when the frame had nothing to submit.
    pub batch: u64,
    /// Textures that the frame deswizzled from guest memory. The caller calls `ResourceTable::synced` for each.
    pub deswizzled: Vec<TexId>,
    /// Textures that the GPU wrote (render targets, copy destinations, dispatch outputs). The caller calls
    /// `ResourceTable::gpu_wrote` for each.
    pub gpu_wrote: Vec<TexId>,
    /// Textures that the frame swizzled back to guest memory for a CPU read. The caller calls
    /// `ResourceTable::synced` for each after `Gpu::wait`.
    pub copied_back: Vec<TexId>,
    /// Textures that the GPU wrote and that the end of the frame copies back into guest memory, because their storage
    /// lies in a memory pool the CPU can access. The copy runs on the GPU after the passes of the frame, so the caller
    /// calls `ResourceTable::synced_at` (after `gpu_wrote_at`) for each without a wait: any later GPU read of the
    /// guest memory, and any CPU read after a fence wait, sees the copy.
    pub mirrored: Vec<TexId>,
    /// The counters.
    pub stats: EncodeStats,
    /// The GPU counters of the frame, which the backend writes when the GPU finished it.
    pub(crate) queries: Option<std::sync::Arc<crate::queries::FrameQueries>>,
}
