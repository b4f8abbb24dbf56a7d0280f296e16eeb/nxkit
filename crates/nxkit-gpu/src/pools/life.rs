//! The pins, the counters and the graveyard of the pool set.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{Backing, Drained, PagePins};

/// A backing that left its buffer and waits for the encoder to drop it.
pub(super) struct Dead<B> {
    pub(super) mem: B,
    pub(super) host_start: u64,
    pub(super) len: u64,
}

impl<B> Dead<B> {
    /// Drops the backing, then unpins its range.
    pub(super) fn release(self, pinner: &Pinner) {
        let (start, len) = (self.host_start, self.len);
        drop(self.mem);
        pinner.unpin(start, len);
    }
}

/// Calls the pins and counts them.
pub(super) struct Pinner {
    pub(super) pins: Arc<dyn PagePins>,
    pub(super) count: AtomicU64,
    pub(super) bytes: AtomicU64,
    /// Backings that exist in a `PoolBuffer`.
    pub(super) alive: AtomicU64,
    pub(super) alive_bytes: AtomicU64,
}

impl Pinner {
    pub(super) fn pin(&self, start: u64, len: u64) {
        self.pins.pin(start, start + len);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(len, Ordering::Relaxed);
    }

    pub(super) fn unpin(&self, start: u64, len: u64) {
        self.pins.unpin(start, start + len);
        self.count.fetch_sub(1, Ordering::Relaxed);
        self.bytes.fetch_sub(len, Ordering::Relaxed);
    }
}

/// Backings whose last owner is gone. The lock is held for a push or a take, never while a backing drops.
pub(super) struct Yard<B: Backing> {
    pub(super) items: Mutex<Vec<Dead<B>>>,
    pub(super) pinner: Arc<Pinner>,
}

impl<B: Backing> Yard<B> {
    pub(super) fn lock(&self) -> MutexGuard<'_, Vec<Dead<B>>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn push(&self, dead: Dead<B>) {
        self.lock().push(dead);
    }

    pub(super) fn drain(&self) -> Drained {
        let items = std::mem::take(&mut *self.lock());
        let mut out = Drained::default();
        for dead in items {
            out.count += 1;
            out.bytes += dead.len;
            dead.release(&self.pinner);
        }
        out
    }
}

impl<B: Backing> Drop for Yard<B> {
    /// A set that goes away leaves nothing in its graveyard. A `PoolBuffer` that outlives the set drops in place.
    fn drop(&mut self) {
        self.drain();
    }
}
