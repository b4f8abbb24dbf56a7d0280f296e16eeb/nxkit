//! Waiter lists per guest address, in a table of small locks. Two addresses in different shards never share a lock.

use crate::thread::{Thread, lock};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

const SHARDS: usize = 64;

/// One blocked thread.
pub(crate) struct WaitEntry {
    pub(crate) thread: Arc<Thread>,
    /// The order of arrival, which breaks ties between equal priorities.
    pub(crate) seq: u64,
    /// For a lock wait, the tag to write when the thread gets the lock. For a condition variable, the same tag.
    pub(crate) tag: u32,
    /// For a condition variable wait, the address of the lock to take again.
    pub(crate) lock_addr: u64,
}

/// The waiters of the addresses in one shard.
#[derive(Default)]
pub(crate) struct Waiters(HashMap<u64, Vec<WaitEntry>>);

impl Waiters {
    pub(crate) fn push(&mut self, key: u64, entry: WaitEntry) {
        self.0.entry(key).or_default().push(entry);
    }

    pub(crate) fn count(&self, key: u64) -> usize {
        self.0.get(&key).map_or(0, Vec::len)
    }

    /// Takes the waiter the kernel wakes first: the lowest priority number, and the earliest at equal priority.
    pub(crate) fn pop_best(&mut self, key: u64) -> Option<WaitEntry> {
        let list = self.0.get_mut(&key)?;
        let best = (0..list.len()).min_by_key(|&i| (list[i].thread.priority(), list[i].seq))?;
        let e = list.swap_remove(best);
        if list.is_empty() {
            self.0.remove(&key);
        }
        Some(e)
    }

    /// Removes the entry of `thread`. False when it is not there, because a waker took it.
    pub(crate) fn remove(&mut self, key: u64, thread: &Arc<Thread>) -> bool {
        let Some(list) = self.0.get_mut(&key) else {
            return false;
        };
        let before = list.len();
        list.retain(|e| !Arc::ptr_eq(&e.thread, thread));
        let removed = list.len() != before;
        if list.is_empty() {
            self.0.remove(&key);
        }
        removed
    }
}

/// A sharded table of waiter lists.
pub(crate) struct WaitTable {
    shards: Vec<Mutex<Waiters>>,
    seq: AtomicU64,
}

impl WaitTable {
    pub(crate) fn new() -> WaitTable {
        WaitTable {
            shards: (0..SHARDS)
                .map(|_| Mutex::new(Waiters::default()))
                .collect(),
            seq: AtomicU64::new(0),
        }
    }

    /// Locks the shard of `key`.
    pub(crate) fn lock(&self, key: u64) -> MutexGuard<'_, Waiters> {
        let h = (key >> 2).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58;
        lock(&self.shards[h as usize % SHARDS])
    }

    pub(crate) fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }
}
