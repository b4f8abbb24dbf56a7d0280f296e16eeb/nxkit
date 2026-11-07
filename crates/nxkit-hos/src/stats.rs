//! Per-thread counters. They are relaxed atomics, cheap enough to stay on.

use crate::role::ThreadRole;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// The live counters of one thread.
#[derive(Default)]
pub(crate) struct ThreadStats {
    pub(crate) waits: AtomicU64,
    pub(crate) wait_ns: AtomicU64,
    pub(crate) wakeups: AtomicU64,
    pub(crate) timeouts: AtomicU64,
    pub(crate) contended_locks: AtomicU64,
    pub(crate) gate_wait_ns: AtomicU64,
    /// Set while the thread is parked in a lock, condition variable or address wait.
    pub(crate) blocked: AtomicBool,
}

impl ThreadStats {
    pub(crate) fn add_wait(&self, ns: u64, signaled: bool) {
        self.waits.fetch_add(1, Relaxed);
        self.wait_ns.fetch_add(ns, Relaxed);
        if signaled {
            self.wakeups.fetch_add(1, Relaxed);
        } else {
            self.timeouts.fetch_add(1, Relaxed);
        }
    }
}

/// A copy of the counters of one thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// The thread id.
    pub thread_id: u64,
    /// The thread name.
    pub name: String,
    /// The role of the thread.
    pub role: ThreadRole,
    /// Blocking waits entered: locks, condition variables, addresses, synchronization and sleeps.
    pub waits: u64,
    /// Time spent in those waits.
    pub wait_ns: u64,
    /// Waits that ended because another thread signalled.
    pub wakeups: u64,
    /// Waits that ended on their timeout.
    pub timeouts: u64,
    /// `ArbitrateLock` calls that had to wait because the lock was owned.
    pub contended_locks: u64,
    /// Time spent waiting for a higher-priority thread of the same core (only with the core rules on).
    pub gate_wait_ns: u64,
}

impl StatsSnapshot {
    pub(crate) fn of(id: u64, name: String, role: ThreadRole, s: &ThreadStats) -> StatsSnapshot {
        StatsSnapshot {
            thread_id: id,
            name,
            role,
            waits: s.waits.load(Relaxed),
            wait_ns: s.wait_ns.load(Relaxed),
            wakeups: s.wakeups.load(Relaxed),
            timeouts: s.timeouts.load(Relaxed),
            contended_locks: s.contended_locks.load(Relaxed),
            gate_wait_ns: s.gate_wait_ns.load(Relaxed),
        }
    }
}

/// A table of the snapshots, one line per thread.
pub fn format_stats(rows: &[StatsSnapshot]) -> String {
    let mut out = String::from(
        "id      role       waits      wait_ms    wakeups  timeouts  contended  gate_ms   name\n",
    );
    for r in rows {
        let _ = writeln!(
            out,
            "{:<7} {:<10} {:<10} {:<10.3} {:<8} {:<9} {:<10} {:<9.3} {}",
            r.thread_id,
            r.role.name(),
            r.waits,
            r.wait_ns as f64 / 1e6,
            r.wakeups,
            r.timeouts,
            r.contended_locks,
            r.gate_wait_ns as f64 / 1e6,
            r.name
        );
    }
    out
}
