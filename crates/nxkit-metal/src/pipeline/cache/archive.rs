//! Writing the captured archive, and handing the capture over to a fresh serializer.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use super::{Generation, PipelineCache};
use crate::error::{MetalError, Result};

impl PipelineCache {
    /// Writes the archive of every pipeline the current serializer captured. The cache must have been made with
    /// `collect`. A long run should use [`PipelineCache::rotate_archive`]: a serializer has a store of limited size.
    pub fn save_archive(&self, path: &Path) -> Result<()> {
        self.generation().write(path)
    }

    /// The pipelines the current serializer has captured, about: the count of pipelines inserted since it was made.
    pub fn captured(&self) -> u64 {
        self.captured.load(Ordering::Relaxed)
    }

    /// Hands the capture over: from now on new compiles run on a new compiler with a fresh serializer, the old
    /// serializer waits (up to 20 s) until the compiles that were running on its compiler are done, and then writes its
    /// archive to `path`. The serializer's internal store is limited (about 10 MB); a full one hands Metal nil data and
    /// the compiler crashes, so the host rotates well before. If compiles are still running after the wait, nothing is
    /// written (their capture is lost, but the serializer is not touched under them).
    pub fn rotate_archive(&self, path: &Path) -> Result<()> {
        if !self.collecting {
            return Err(MetalError::Invalid("the cache was made without `collect`"));
        }
        let fresh = Arc::new(Generation::new(&self.ctx, true)?);
        let old = std::mem::replace(
            &mut *self.current.write().unwrap_or_else(PoisonError::into_inner),
            fresh,
        );
        self.captured.store(0, Ordering::Relaxed);
        let t0 = Instant::now();
        while Arc::strong_count(&old) > 1 && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let quiet = Arc::strong_count(&old) == 1;
        let written = if quiet {
            old.write(path)
        } else {
            Err(MetalError::Invalid("compiles were still running"))
        };
        self.retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(old);
        written
    }
}
