//! The memory pool calls of the game that change [`Gpu`], without a wait for the lock of the encoder.
//!
//! A game makes and finalizes memory pools every frame (Ocarina of Time about 10), and the lock of `Gpu` is held for the
//! whole encode of a frame. What these calls change in `Gpu` (the copy-back pools and windows, the raw global memory
//! pools) is bookkeeping that only the encoder reads, under that lock. So a call that finds the lock taken puts its op
//! into [`PoolOps`] and returns, and whoever takes the lock next applies the queued ops first, in the order they were
//! queued (`GpuBackend::lock_gpu`).
//!
//! Order: an op is applied directly only by a thread that holds the lock of `Gpu` and has applied the whole queue
//! first, and a queued op stays behind every op queued before it. So the ops of one thread reach `Gpu` in the order of
//! the calls, and an op that returned before a flush is in `Gpu` before that frame is encoded (the encode takes the lock
//! and drains the queue). The check that can fail runs before an op is made (`GpuBackend::register_pool`), so applying
//! an op does not fail.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

use crate::Gpu;

/// A change to the pools of [`Gpu`], in GPU addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PoolOp {
    /// `Gpu::register_memory_pool`.
    Register { addr: u64, len: u64, flags: u32 },
    /// `Gpu::unregister_memory_pool`.
    Unregister { addr: u64 },
    /// `Gpu::map_pool_window`.
    Map { addr: u64, len: u64, flags: u32 },
    /// `Gpu::unmap_pool_window`.
    Unmap { addr: u64, len: u64 },
}

/// What a [`PoolOp`] changes. `Gpu` in the backend, a list in the tests.
pub(super) trait PoolTarget {
    fn apply(&mut self, op: PoolOp);
}

impl PoolTarget for Gpu {
    fn apply(&mut self, op: PoolOp) {
        match op {
            PoolOp::Register { addr, len, flags } => {
                // The range was checked before the op was made, against the same address map.
                if let Err(e) = self.register_memory_pool(addr, len, flags) {
                    eprintln!(
                        "nxkit-gpu: a queued pool {addr:#x}+{len:#x} was not registered: {e}"
                    );
                }
            }
            PoolOp::Unregister { addr } => self.unregister_memory_pool(addr),
            PoolOp::Map { addr, len, flags } => self.map_pool_window(addr, len, flags),
            PoolOp::Unmap { addr, len } => self.unmap_pool_window(addr, len),
        }
    }
}

/// The pool ops that found the lock of `Gpu` taken, in the order they came.
#[derive(Default)]
pub(super) struct PoolOps {
    queue: Mutex<Vec<PoolOp>>,
    /// Ops applied by the caller (`NXKIT_DIAG=pools`).
    direct: AtomicU64,
    /// Ops that went into the queue.
    deferred: AtomicU64,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PoolOps {
    /// Applies `op` to the target when its lock is free, and queues it otherwise.
    pub(super) fn run<T: PoolTarget>(&self, target: &Mutex<T>, op: PoolOp) {
        let mut t = match target.try_lock() {
            Ok(t) => t,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => {
                locked(&self.queue).push(op);
                self.deferred.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        // What others queued came first.
        self.drain(&mut *t);
        t.apply(op);
        self.direct.fetch_add(1, Ordering::Relaxed);
    }

    /// Locks the target and applies the queued ops to it.
    pub(super) fn lock<'a, T: PoolTarget>(&self, target: &'a Mutex<T>) -> MutexGuard<'a, T> {
        let mut t = locked(target);
        self.drain(&mut *t);
        t
    }

    /// Applies the queued ops to the target, whose lock the caller holds. The queue's own lock is not held while they
    /// are applied, so a call that queues does not wait for them.
    fn drain<T: PoolTarget>(&self, t: &mut T) {
        let ops = std::mem::take(&mut *locked(&self.queue));
        for op in ops {
            t.apply(op);
        }
    }

    /// The ops applied by their caller and the ops that were queued, since the start.
    pub(super) fn counts(&self) -> (u64, u64) {
        (
            self.direct.load(Ordering::Relaxed),
            self.deferred.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl PoolTarget for Vec<PoolOp> {
        fn apply(&mut self, op: PoolOp) {
            self.push(op);
        }
    }

    const REG: PoolOp = PoolOp::Register {
        addr: 0x1000,
        len: 0x100,
        flags: 8,
    };
    const UNREG: PoolOp = PoolOp::Unregister { addr: 0x1000 };
    const MAP: PoolOp = PoolOp::Map {
        addr: 0x1000,
        len: 0x40,
        flags: 1,
    };
    const UNMAP: PoolOp = PoolOp::Unmap {
        addr: 0x1000,
        len: 0x40,
    };

    #[test]
    fn a_free_lock_applies_the_op_at_once() {
        let ops = PoolOps::default();
        let target = Mutex::new(Vec::new());
        ops.run(&target, REG);
        assert_eq!(*locked(&target), [REG]);
        assert_eq!(ops.counts(), (1, 0));
    }

    #[test]
    fn ops_that_find_the_lock_taken_wait_in_order_for_the_next_lock() {
        let ops = PoolOps::default();
        let target = Mutex::new(Vec::new());
        {
            let held = locked(&target);
            for op in [REG, MAP, UNMAP, UNREG] {
                ops.run(&target, op);
            }
            assert!(held.is_empty());
        }
        assert_eq!(ops.counts(), (0, 4));
        // The encoder takes the lock: it sees every op, register before unregister.
        assert_eq!(*ops.lock(&target), [REG, MAP, UNMAP, UNREG]);
        // The queue is empty now; nothing is applied twice.
        assert_eq!(ops.lock(&target).len(), 4);
    }

    #[test]
    fn an_op_with_a_free_lock_goes_behind_the_queued_ones() {
        let ops = PoolOps::default();
        let target = Mutex::new(Vec::new());
        {
            let _held = locked(&target);
            ops.run(&target, REG);
        }
        ops.run(&target, UNREG);
        assert_eq!(*locked(&target), [REG, UNREG]);
        assert_eq!(ops.counts(), (1, 1));
    }

    #[test]
    fn ops_from_threads_while_the_lock_is_held_keep_the_order_of_each_thread() {
        let ops = PoolOps::default();
        let target = Mutex::new(Vec::new());
        let held = locked(&target);
        std::thread::scope(|s| {
            for t in 0..4u64 {
                let (ops, target) = (&ops, &target);
                s.spawn(move || {
                    for i in 0..100u64 {
                        let addr = (t << 32) | (i << 12);
                        ops.run(
                            target,
                            PoolOp::Register {
                                addr,
                                len: 1,
                                flags: 0,
                            },
                        );
                        ops.run(target, PoolOp::Unregister { addr });
                    }
                });
            }
        });
        drop(held);
        let seen = ops.lock(&target).clone();
        assert_eq!(seen.len(), 800);
        for t in 0..4u64 {
            let mine: Vec<_> = seen
                .iter()
                .filter(|op| match op {
                    PoolOp::Register { addr, .. } | PoolOp::Unregister { addr } => addr >> 32 == t,
                    _ => false,
                })
                .collect();
            let want: Vec<_> = (0..100u64)
                .flat_map(|i| {
                    let addr = (t << 32) | (i << 12);
                    [
                        PoolOp::Register {
                            addr,
                            len: 1,
                            flags: 0,
                        },
                        PoolOp::Unregister { addr },
                    ]
                })
                .collect();
            assert_eq!(mine.into_iter().copied().collect::<Vec<_>>(), want);
        }
    }
}
