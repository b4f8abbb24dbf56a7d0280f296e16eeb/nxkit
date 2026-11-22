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

impl EncodeReport {
    /// The textures whose GPU copy and guest bytes are the same at the end of the frame: the uploads that no pass of the
    /// frame wrote afterwards, and the textures copied back. The uploads run before the passes, so a texture that a pass
    /// wrote after its upload stays newer on the GPU. Pokopia makes a new texture for its fog colour curves, renders
    /// them into it and reads them back with `nvnTextureReadTexels`.
    pub fn synced(&self) -> Vec<TexId> {
        let wrote: crate::fastmap::FastSet<TexId> = self.gpu_wrote.iter().copied().collect();
        self.deswizzled
            .iter()
            .filter(|id| !wrote.contains(id))
            .chain(&self.mirrored)
            .chain(&self.copied_back)
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_texture_written_after_its_upload_is_not_synced() {
        let (up, drawn, mirrored) = (TexId(1), TexId(2), TexId(3));
        let report = EncodeReport {
            deswizzled: vec![up, drawn],
            gpu_wrote: vec![drawn, mirrored],
            mirrored: vec![mirrored],
            ..EncodeReport::default()
        };
        assert_eq!(report.synced(), vec![up, mirrored]);
    }
}
