//! The counters of the pipeline cache, and closing the archive shards once the prewarm is over.

use std::sync::atomic::Ordering;

use super::PipelineCache;

impl PipelineCache {
    /// The number of lookups that found a pipeline.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// The number of pipelines built through the compiler (an archive hit counts too).
    pub fn builds(&self) -> u64 {
        self.builds.load(Ordering::Relaxed)
    }

    /// True when an archive was found and attached: the single archive or the index of the shards.
    pub fn has_archive(&self) -> bool {
        self.loaded_archive
    }

    /// The shards of the archive: (files in the index, pipelines in it, open now, opened since the start). None when
    /// the cache has no shards.
    pub fn shard_stats(&self) -> Option<(usize, usize, usize, u64)> {
        self.shards
            .as_ref()
            .map(|s| (s.shards(), s.pipelines(), s.open_now(), s.opens()))
    }

    /// Closes every open archive shard. `newArchiveWithURL` keeps the whole file in malloc memory, and once the
    /// prewarm made the pipelines of the list the shards sit idle: they are the first memory that the system
    /// compresses, and a later lookup then waits for the decompression. A later lookup opens its shard again.
    /// Returns how many were open.
    pub fn close_shards(&self) -> usize {
        self.shards.as_ref().map_or(0, |s| s.close_all())
    }
}
