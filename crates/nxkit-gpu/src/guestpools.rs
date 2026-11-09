//! The pool set as the encoder uses it: the backing of a pool is a shared [`GuestMemory`].
//!
//! The backend, the encoder and the host share one `Arc<GuestMemory>`, so the whole mode wraps that `Arc`. The per-pool
//! mode makes one `Arc<GuestMemory>` per pool, so both modes have one snapshot type and every resolve site is the same.

use std::sync::Arc;

use nxkit_metal::GuestMemory;

use crate::pools::{Backing, PagePins, PoolSet, PoolSnapshot};

/// The pools of one moment, with shared Metal buffers as backings.
pub type GuestPools = PoolSnapshot<Arc<GuestMemory>>;

/// The set that owns the current [`GuestPools`].
pub(crate) type GuestPoolSet = PoolSet<Arc<GuestMemory>>;

impl Backing for Arc<GuestMemory> {
    fn real_base(&self) -> u64 {
        self.address_map().gpu_base()
    }
}

/// The pins of the whole mode: the single buffer is pinned for the whole run by the host (`nxkit_hos::pinned`), so the
/// set has nothing to pin.
pub(crate) struct NoPins;

impl PagePins for NoPins {
    fn pin(&self, _start: u64, _end: u64) {}

    fn unpin(&self, _start: u64, _end: u64) {}
}

/// The set of the whole mode over `guest`: one pool over the whole range, at the real address of the buffer, so
/// `resolve` gives back the address it was given.
pub(crate) fn whole(guest: &Arc<GuestMemory>) -> GuestPoolSet {
    let base = guest.address_map().gpu_base();
    PoolSet::whole(base, guest.len(), Arc::clone(guest), Arc::new(NoPins))
}
