//! The QoS lend of a waiter to the owner of a lock, safe against an owner that exits.
//!
//! The first version kept the owner's `pthread_t` in an atomic and the waiter called
//! `pthread_override_qos_class_start_np` with it. An owner that exited between the read and the call left the waiter
//! with a dangling `pthread_t`. The lend is now a small table that lives in the owner's thread record, which every
//! waiter holds through an `Arc<Thread>`, so the record outlives every lend. The table has one lock. The owner opens
//! it with its host thread when it starts and closes it when it exits, on its own host thread that still runs. A
//! waiter starts an override only while the table is open, and it does so under the table's lock. `close` takes the
//! same lock, ends every override that is still pending and marks the table closed. A start after the close finds the
//! table closed and does nothing, and a start before the close has finished before `close` runs. So no override
//! starts on a thread that is gone, and none stays behind. The lock is held for one pthread call, never across a wait,
//! and it is per thread, so it is not a global lock (R7).
//!
//! Not verified: whether `pthread_override_qos_class_end_np` may run on a thread other than the one that started it.
//! The waiter ends its override on its own thread, which started it, so the normal path does not depend on it. Only
//! `close` ends an override that another thread started, and only for a waiter that is still waiting.

use crate::role::Qos;
use crate::sys;
use crate::thread::{Thread, lock};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct Inner {
    host: usize,
    closed: bool,
    live: HashMap<u64, sys::QosOverride>,
}

/// The lend table of one thread.
#[derive(Default)]
pub(crate) struct Lend {
    inner: Mutex<Inner>,
    next: AtomicU64,
}

impl Lend {
    /// The thread runs on `host` from now on. Called by the thread itself.
    pub(crate) fn open(&self, host: usize) {
        let mut g = lock(&self.inner);
        g.host = host;
        g.closed = false;
    }

    /// Starts an override of the owner's host thread at `qos`. `None` when the table is closed or not opened, or
    /// when the platform has no overrides. The returned ticket ends the override.
    pub(crate) fn start(&self, qos: Qos) -> Option<u64> {
        let mut g = lock(&self.inner);
        if g.closed || g.host == 0 {
            return None;
        }
        let over = sys::QosOverride::start(g.host, qos)?;
        let ticket = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        g.live.insert(ticket, over);
        Some(ticket)
    }

    /// Ends the override of `ticket`. Nothing happens when `close` has ended it already.
    pub(crate) fn end(&self, ticket: u64) {
        let over = lock(&self.inner).live.remove(&ticket);
        drop(over);
    }

    /// Ends every pending override and refuses new ones. Called by the owner on its own thread when it exits.
    pub(crate) fn close(&self) {
        let mut g = lock(&self.inner);
        g.closed = true;
        g.host = 0;
        g.live.clear();
    }
}

/// An override that ends when the guard drops. The guard holds the owner's thread record, so the record is alive
/// when the guard ends the override.
pub(crate) struct LendGuard {
    owner: Arc<Thread>,
    ticket: u64,
}

impl LendGuard {
    pub(crate) fn start(owner: Arc<Thread>, qos: Qos) -> Option<LendGuard> {
        let ticket = owner.lend.start(qos)?;
        Some(LendGuard { owner, ticket })
    }
}

impl Drop for LendGuard {
    fn drop(&mut self) {
        self.owner.lend.end(self.ticket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_or_unopened_table_starts_nothing() {
        let l = Lend::default();
        assert!(l.start(Qos::UserInteractive).is_none());
        l.open(1);
        l.close();
        assert!(l.start(Qos::UserInteractive).is_none());
        l.end(7);
    }
}
