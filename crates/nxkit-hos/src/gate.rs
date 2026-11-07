//! The core rules of Horizon, off by default: a thread runs before every lower-priority thread of its core.
//!
//! A thread counts as runnable on its core between `enter` and `leave`. `enter` blocks while a thread of higher
//! priority is runnable on the same core. The core's gate is one word that holds the best runnable priority. A
//! blocked thread waits on that word and is woken when it changes, so nothing polls.
//!
//! The gate acts at the points where a guest thread starts, wakes up or blocks. It cannot stop a thread that runs
//! guest code without a svc, and it does not move threads between cores.

use crate::sys;
use crate::thread::{Thread, lock};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

const PRIORITIES: usize = 64;
/// The value of the gate word when no thread is runnable.
const IDLE: u32 = PRIORITIES as u32;

struct CoreGate {
    best: AtomicU32,
    counts: Mutex<[u32; PRIORITIES]>,
}

impl CoreGate {
    fn publish(&self, counts: &[u32; PRIORITIES]) {
        let best = counts
            .iter()
            .position(|n| *n > 0)
            .map_or(IDLE, |i| i as u32);
        if self.best.swap(best, Ordering::SeqCst) != best {
            sys::wake_all(&self.best);
        }
    }
}

/// The four core gates.
pub(crate) struct CoreGates {
    enabled: bool,
    cores: [CoreGate; 4],
}

fn clamp(p: i32) -> usize {
    p.clamp(0, PRIORITIES as i32 - 1) as usize
}

impl CoreGates {
    pub(crate) fn new(enabled: bool) -> CoreGates {
        let gate = || CoreGate {
            best: AtomicU32::new(IDLE),
            counts: Mutex::new([0; PRIORITIES]),
        };
        CoreGates {
            enabled,
            cores: [gate(), gate(), gate(), gate()],
        }
    }

    /// Marks `t` runnable and waits until no higher-priority thread is runnable on its core.
    pub(crate) fn enter(&self, t: &Thread) {
        if !self.enabled {
            return;
        }
        let core = t.core().min(3);
        let gate = &self.cores[core];
        let p = {
            let mut slot = lock(&t.gate_slot);
            if slot.is_some() {
                return;
            }
            let p = clamp(t.priority());
            let mut counts = lock(&gate.counts);
            counts[p] += 1;
            gate.publish(&counts);
            *slot = Some((core, p as i32));
            p as u32
        };
        let t0 = Instant::now();
        let mut waited = false;
        loop {
            let best = gate.best.load(Ordering::SeqCst);
            if best >= p {
                break;
            }
            waited = true;
            sys::wait_u32(&gate.best, best, None);
        }
        if waited {
            let ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
            t.stats.gate_wait_ns.fetch_add(ns, Ordering::Relaxed);
        }
    }

    /// Marks `t` as not runnable, because it blocks or exits.
    pub(crate) fn leave(&self, t: &Thread) {
        if !self.enabled {
            return;
        }
        let mut slot = lock(&t.gate_slot);
        if let Some((core, p)) = slot.take() {
            let gate = &self.cores[core];
            let mut counts = lock(&gate.counts);
            counts[p as usize] -= 1;
            gate.publish(&counts);
        }
    }

    /// Changes the priority of `t`. A runnable thread moves to the new priority in its core's counts.
    pub(crate) fn set_priority(&self, t: &Thread, priority: i32) {
        let mut slot = lock(&t.gate_slot);
        t.priority.store(priority, Ordering::Relaxed);
        if let (true, Some((core, old))) = (self.enabled, *slot) {
            let gate = &self.cores[core];
            let new = clamp(priority);
            let mut counts = lock(&gate.counts);
            counts[old as usize] -= 1;
            counts[new] += 1;
            gate.publish(&counts);
            *slot = Some((core, new as i32));
        }
    }
}
