//! Process-wide counters of the Metal memory this crate made, for the memory report of the host
//! (`NXKIT_MEM_REPORT`).
//!
//! Each allocation carries a [`Tracked`] that adds its bytes when it is made and takes them off when it drops. The bytes
//! are `allocatedSize` of the Metal object: what the device really took, with alignment and padding. Views and textures
//! of a drawable count nothing, since they own no memory.

use std::sync::atomic::{AtomicU64, Ordering};

/// What a counter counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A texture with its own storage (private, shared or an IOSurface).
    Texture,
    /// A buffer that Metal allocated: data buffers and the chunks of the scratch allocators.
    Buffer,
    /// The no-copy buffer over guest memory. Its bytes are the length of the range, not memory that Metal took.
    Guest,
    /// A shader library (a `.metallib` that was loaded or MSL that was compiled). Its bytes are unknown, so 0.
    Library,
    /// A pipeline archive file that is open (the single archive or a shard). Its bytes are the file size: the archive
    /// reads its file into memory.
    Shard,
}

const KINDS: usize = 5;

static COUNT: [AtomicU64; KINDS] = [const { AtomicU64::new(0) }; KINDS];
static BYTES: [AtomicU64; KINDS] = [const { AtomicU64::new(0) }; KINDS];
static PIPELINES: AtomicU64 = AtomicU64::new(0);
static EVICTIONS: AtomicU64 = AtomicU64::new(0);
static EVICTED_BYTES: AtomicU64 = AtomicU64::new(0);
static REUPLOADS: AtomicU64 = AtomicU64::new(0);
static SHARD_OPENS: AtomicU64 = AtomicU64::new(0);

/// One live allocation in the counters. Dropping it takes it out again.
#[derive(Debug)]
pub(crate) struct Tracked {
    kind: Kind,
    bytes: u64,
}

impl Tracked {
    pub(crate) fn new(kind: Kind, bytes: u64) -> Self {
        COUNT[kind as usize].fetch_add(1, Ordering::Relaxed);
        BYTES[kind as usize].fetch_add(bytes, Ordering::Relaxed);
        Self { kind, bytes }
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        COUNT[self.kind as usize].fetch_sub(1, Ordering::Relaxed);
        BYTES[self.kind as usize].fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Counts a pipeline state that a [`crate::PipelineCache`] keeps. The caches never drop one.
pub(crate) fn pipeline_kept() {
    PIPELINES.fetch_add(1, Ordering::Relaxed);
}

/// Counts the opening of a pipeline archive file. A shard that was closed and is asked again counts again.
pub(crate) fn shard_opened() {
    SHARD_OPENS.fetch_add(1, Ordering::Relaxed);
}

/// Counts a texture that the encoder dropped because it was not used for a while and its guest copy is current.
pub fn texture_evicted(bytes: u64) {
    EVICTIONS.fetch_add(1, Ordering::Relaxed);
    EVICTED_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

/// Counts a texture that was made again, and uploaded again, after an eviction.
pub fn texture_reuploaded() {
    REUPLOADS.fetch_add(1, Ordering::Relaxed);
}

/// The live Metal objects of this crate, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemStats {
    /// Textures with their own storage.
    pub textures: u64,
    /// Their `allocatedSize` in bytes.
    pub texture_bytes: u64,
    /// Data buffers and scratch chunks.
    pub buffers: u64,
    /// Their `allocatedSize` in bytes.
    pub buffer_bytes: u64,
    /// No-copy guest buffers (one per run).
    pub guest_buffers: u64,
    /// Their length in bytes. `currentAllocatedSize` of the device counts it whole, touched or not.
    pub guest_bytes: u64,
    /// Shader libraries that are alive.
    pub libraries: u64,
    /// Pipeline states that the pipeline caches hold.
    pub pipelines: u64,
    /// Textures dropped for not being used (since the start).
    pub evictions: u64,
    /// Their `allocatedSize` in bytes.
    pub evicted_bytes: u64,
    /// Evicted textures that were made again.
    pub reuploads: u64,
    /// Pipeline archive files that are open: the single archive or the shards that the cache keeps.
    pub archive_shards: u64,
    /// Their file sizes in bytes.
    pub archive_shard_bytes: u64,
    /// Archive files opened since the start (a closed shard that was asked again counts again).
    pub archive_opens: u64,
}

/// The counters now.
pub fn mem_stats() -> MemStats {
    let c = |k: Kind| COUNT[k as usize].load(Ordering::Relaxed);
    let b = |k: Kind| BYTES[k as usize].load(Ordering::Relaxed);
    MemStats {
        textures: c(Kind::Texture),
        texture_bytes: b(Kind::Texture),
        buffers: c(Kind::Buffer),
        buffer_bytes: b(Kind::Buffer),
        guest_buffers: c(Kind::Guest),
        guest_bytes: b(Kind::Guest),
        libraries: c(Kind::Library),
        pipelines: PIPELINES.load(Ordering::Relaxed),
        evictions: EVICTIONS.load(Ordering::Relaxed),
        evicted_bytes: EVICTED_BYTES.load(Ordering::Relaxed),
        reuploads: REUPLOADS.load(Ordering::Relaxed),
        archive_shards: c(Kind::Shard),
        archive_shard_bytes: b(Kind::Shard),
        archive_opens: SHARD_OPENS.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tracked_allocation_counts_while_it_lives() {
        let before = mem_stats();
        let t = Tracked::new(Kind::Library, 0);
        assert!(mem_stats().libraries > before.libraries);
        drop(t);
        // Other tests make libraries in parallel, so only the own one is checked by the change it makes.
        let t = Tracked::new(Kind::Buffer, 1 << 40);
        assert!(mem_stats().buffer_bytes >= 1 << 40);
        drop(t);
        assert!(mem_stats().buffer_bytes < 1 << 40);
    }

    #[test]
    fn an_open_shard_counts_its_file_size_while_it_lives() {
        let before = mem_stats();
        let t = Tracked::new(Kind::Shard, 1 << 41);
        let now = mem_stats();
        assert!(now.archive_shards > before.archive_shards);
        assert!(now.archive_shard_bytes >= 1 << 41);
        drop(t);
        assert!(mem_stats().archive_shard_bytes < 1 << 41);
        shard_opened();
        assert!(mem_stats().archive_opens > before.archive_opens);
    }

    #[test]
    fn evictions_and_reuploads_add_up() {
        let before = mem_stats();
        texture_evicted(1000);
        texture_reuploaded();
        let after = mem_stats();
        assert!(after.evictions > before.evictions);
        assert!(after.evicted_bytes >= before.evicted_bytes + 1000);
        assert!(after.reuploads > before.reuploads);
    }
}
