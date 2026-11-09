//! The batch counter of a progress display.

use crate::Config;

/// Batches compiled so far and in all, for a caller's progress display: one atomic add for each batch.
#[derive(Debug, Default)]
pub struct Batches {
    /// Batches done (compiled, or taken from the store).
    pub done: std::sync::atomic::AtomicU64,
    /// Batches in all. A streamed archive counts one batch for each group when it starts and corrects the count of a
    /// group when the group is planned (a group is one batch unless it is cut further or holds no pipeline).
    pub total: std::sync::atomic::AtomicU64,
}

impl Batches {
    pub(crate) fn planned(c: &Config, n: usize) {
        if let Some(b) = &c.progress {
            b.total
                .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// A group planned `n` batches where [`Batches::planned`] had counted `estimate` for it at the start.
    pub(crate) fn replanned(c: &Config, estimate: usize, n: usize) {
        if let Some(b) = &c.progress {
            use std::sync::atomic::Ordering::Relaxed;
            if n >= estimate {
                b.total.fetch_add((n - estimate) as u64, Relaxed);
            } else {
                b.total.fetch_sub((estimate - n) as u64, Relaxed);
            }
        }
    }

    pub(crate) fn finished(c: &Config) {
        if let Some(b) = &c.progress {
            b.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
