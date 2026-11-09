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
}

const KINDS: usize = 4;

static COUNT: [AtomicU64; KINDS] = [const { AtomicU64::new(0) }; KINDS];
static BYTES: [AtomicU64; KINDS] = [const { AtomicU64::new(0) }; KINDS];
static PIPELINES: AtomicU64 = AtomicU64::new(0);

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
}
