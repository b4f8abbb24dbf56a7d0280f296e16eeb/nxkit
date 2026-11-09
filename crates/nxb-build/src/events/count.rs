//! The count that workers raise while a step runs.

use std::sync::atomic::{AtomicU64, Ordering};

/// A count that workers raise with one atomic add per item (or per chunk) and [`super::Progress::tracked`] reads: no lock and
/// no allocation in the hot loop.
#[derive(Debug, Default)]
pub struct Count {
    /// Items (or bytes) done.
    pub done: AtomicU64,
    /// Items (or bytes) in all, 0 while unknown.
    pub total: AtomicU64,
}

impl Count {
    /// `n` more done.
    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// The total, once it is known.
    pub fn set_total(&self, n: u64) {
        self.total.store(n, Ordering::Relaxed);
    }

    /// `n` more in all.
    pub fn add_total(&self, n: u64) {
        self.total.fetch_add(n, Ordering::Relaxed);
    }
}
