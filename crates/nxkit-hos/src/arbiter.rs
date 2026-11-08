//! Locks, condition variables and address waits, on words in guest memory.
//!
//! A blocked thread waits on its own parker word, because a lock is handed to one chosen thread and a plain wake of
//! an address wakes any one. The guest word is never waited on directly. The wait lists are per address in sharded
//! tables. The lock order is condition variable shard, then lock shard. Nothing takes them the other way round.
//!
//! The behaviour follows nxnative's `hle_thread.c` (`ArbitrateLock` line 1029, `WaitProcessWideKeyAtomic` 1044,
//! `SignalProcessWideKey` 1070, `WaitForAddress` 1080, `SignalToAddress` 1093) and Horizon's kernel.

use crate::kernel::Kernel;
use crate::result::{KResult, KResultOr};
use crate::thread::{Thread, Timeout};
use crate::waitq::{WaitEntry, Waiters};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The bit of a lock word that says threads wait for the lock. The rest of the word is the owner's handle.
pub const WAITERS_BIT: u32 = 0x4000_0000;

/// How `WaitForAddress` tests the word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArbitrationType {
    /// Wait if the 32-bit value is less than the argument.
    LessThan,
    /// If the value is less than the argument, decrement it and wait.
    DecrementAndWaitIfLessThan,
    /// Wait if the 32-bit value equals the argument.
    Equal,
    /// Wait if the 64-bit value equals the argument (Horizon 18).
    Equal64,
}

impl ArbitrationType {
    /// From the svc argument.
    pub fn from_raw(raw: u32) -> KResultOr<ArbitrationType> {
        match raw {
            0 => Ok(ArbitrationType::LessThan),
            1 => Ok(ArbitrationType::DecrementAndWaitIfLessThan),
            2 => Ok(ArbitrationType::Equal),
            3 => Ok(ArbitrationType::Equal64),
            _ => Err(KResult::INVALID_ENUM_VALUE),
        }
    }
}

/// How `SignalToAddress` changes the word before it wakes threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalType {
    /// Only wake.
    Signal,
    /// If the value equals the argument, add one, then wake.
    SignalAndIncrementIfEqual,
    /// If the value equals the argument, change it by the number of waiters, then wake.
    SignalAndModifyByWaitingCountIfEqual,
}

impl SignalType {
    /// From the svc argument.
    pub fn from_raw(raw: u32) -> KResultOr<SignalType> {
        match raw {
            0 => Ok(SignalType::Signal),
            1 => Ok(SignalType::SignalAndIncrementIfEqual),
            2 => Ok(SignalType::SignalAndModifyByWaitingCountIfEqual),
            _ => Err(KResult::INVALID_ENUM_VALUE),
        }
    }
}

fn aligned(addr: u64, to: u64) -> KResultOr<()> {
    if addr.is_multiple_of(to) {
        Ok(())
    } else {
        Err(KResult::INVALID_ADDRESS)
    }
}

impl Kernel {
    fn load32(&self, addr: u64) -> KResultOr<u32> {
        self.mem.load32(addr).ok_or(KResult::INVALID_CURRENT_MEMORY)
    }

    fn entry(
        &self,
        table: &crate::waitq::WaitTable,
        cur: &Arc<Thread>,
        tag: u32,
        lock_addr: u64,
    ) -> WaitEntry {
        WaitEntry {
            thread: cur.clone(),
            seq: table.next_seq(),
            tag,
            lock_addr,
        }
    }

    /// Compare and swap that fails with the kernel's results. `Ok(true)` swapped.
    fn cas(&self, addr: u64, current: u32, new: u32) -> KResultOr<bool> {
        match self.mem.cas32(addr, current, new) {
            Some(r) => Ok(r.is_ok()),
            None => Err(KResult::INVALID_CURRENT_MEMORY),
        }
    }

    /// `ArbitrateLock`: the lock word holds `owner | WAITERS_BIT` and the caller waits for the owner to unlock. A
    /// word that has changed already returns at once, and the caller looks at the lock again.
    pub fn arbitrate_lock(
        &self,
        cur: &Arc<Thread>,
        owner: u32,
        addr: u64,
        tag: u32,
    ) -> KResultOr<()> {
        self.svc_enter(cur);
        aligned(addr, 4)?;
        // A kernel handle must name a thread, as on the console. A smaller value is an SDK thread tag: the lock word
        // alone decides whether to wait, and the tag finds its thread only for lending the priority.
        if crate::handle::is_handle(owner) {
            self.thread_by_handle(owner)?;
        }
        self.learn_tag(cur, tag);
        cur.parker.prepare();
        let mut g = self.locks.lock(addr);
        if self.load32(addr)? != owner | WAITERS_BIT {
            return Ok(());
        }
        g.push(addr, self.entry(&self.locks, cur, tag, addr));
        drop(g);
        cur.stats.contended_locks.fetch_add(1, Ordering::Relaxed);
        let _boost = self.boost_owner(cur, owner);
        self.block_on(cur, Timeout::Infinite, || true);
        Ok(())
    }

    /// Gives the lock at `addr` to the best waiter, or frees it. The caller holds the lock shard.
    fn release_lock(&self, g: &mut Waiters, addr: u64) -> KResultOr<()> {
        let Some(w) = g.pop_best(addr) else {
            return if self.mem.store32(addr, 0) {
                Ok(())
            } else {
                Err(KResult::INVALID_CURRENT_MEMORY)
            };
        };
        let more = if g.count(addr) > 0 { WAITERS_BIT } else { 0 };
        let stored = self.mem.store32(addr, w.tag | more);
        w.thread.parker.wake(if stored {
            KResult::SUCCESS
        } else {
            KResult::INVALID_CURRENT_MEMORY
        });
        Ok(())
    }

    /// `ArbitrateUnlock`: the owner hands the lock to the waiter of highest priority, or frees it.
    pub fn arbitrate_unlock(&self, addr: u64) -> KResultOr<()> {
        aligned(addr, 4)?;
        let mut g = self.locks.lock(addr);
        self.release_lock(&mut g, addr)
    }

    /// `WaitProcessWideKeyAtomic`: releases the lock at `lock_addr`, waits on `key` and, when signalled, returns
    /// with the lock taken again for `tag`. The thread joins the wait list before the lock is released, so a signal
    /// that follows the release cannot be missed. On a timeout the lock is not taken again.
    pub fn wait_process_wide_key_atomic(
        &self,
        cur: &Arc<Thread>,
        lock_addr: u64,
        key: u64,
        tag: u32,
        timeout: Timeout,
    ) -> KResultOr<()> {
        self.svc_enter(cur);
        aligned(lock_addr, 4)?;
        aligned(key, 4)?;
        self.learn_tag(cur, tag);
        cur.parker.prepare();
        let mut cg = self.condvars.lock(key);
        if !self.mem.store32(key, 1) {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        {
            let mut lg = self.locks.lock(lock_addr);
            self.release_lock(&mut lg, lock_addr)?;
        }
        if timeout == Timeout::Immediate {
            return Err(KResult::TIMED_OUT);
        }
        cg.push(key, self.entry(&self.condvars, cur, tag, lock_addr));
        drop(cg);
        let signaled = self.block_on(cur, timeout, || self.condvars.lock(key).remove(key, cur));
        match (signaled, cur.parker.result()) {
            (false, _) => Err(KResult::TIMED_OUT),
            (true, KResult::SUCCESS) => Ok(()),
            (true, e) => Err(e),
        }
    }

    /// Turns a woken condition variable waiter into an owner of its lock, or into a waiter for it. The caller holds
    /// the condition variable shard.
    fn lock_for_signaled(&self, w: WaitEntry) {
        let addr = w.lock_addr;
        let mut lg = self.locks.lock(addr);
        loop {
            let v = match self.mem.load32(addr) {
                Some(v) => v,
                None => return w.thread.parker.wake(KResult::INVALID_CURRENT_MEMORY),
            };
            let swapped = if v == 0 {
                match self.mem.cas32(addr, 0, w.tag) {
                    Some(Ok(_)) => return w.thread.parker.wake(KResult::SUCCESS),
                    Some(Err(_)) => continue,
                    None => return w.thread.parker.wake(KResult::INVALID_CURRENT_MEMORY),
                }
            } else if v & WAITERS_BIT != 0 {
                true
            } else {
                matches!(self.mem.cas32(addr, v, v | WAITERS_BIT), Some(Ok(_)))
            };
            if swapped {
                break;
            }
        }
        // The thread waits for the lock now. It queues after the threads that wait already.
        let seq = self.locks.next_seq();
        w.thread
            .stats
            .contended_locks
            .fetch_add(1, Ordering::Relaxed);
        lg.push(addr, WaitEntry { seq, ..w });
    }

    /// `SignalProcessWideKey`: wakes `count` waiters of `key`, all of them for a count of zero or less.
    pub fn signal_process_wide_key(&self, key: u64, count: i32) -> KResultOr<()> {
        aligned(key, 4)?;
        let mut cg = self.condvars.lock(key);
        let mut woken = 0;
        while count <= 0 || woken < count {
            let Some(w) = cg.pop_best(key) else { break };
            self.lock_for_signaled(w);
            woken += 1;
        }
        if cg.count(key) == 0 && !self.mem.store32(key, 0) {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        Ok(())
    }

    /// `WaitForAddress`. For the 32-bit types only the low 32 bits of `value` count.
    pub fn wait_for_address(
        &self,
        cur: &Arc<Thread>,
        addr: u64,
        ty: ArbitrationType,
        value: i64,
        timeout: Timeout,
    ) -> KResultOr<()> {
        self.svc_enter(cur);
        aligned(addr, if ty == ArbitrationType::Equal64 { 8 } else { 4 })?;
        cur.parker.prepare();
        let mut g = self.addresses.lock(addr);
        let v32 = value as i32;
        let ok = match ty {
            ArbitrationType::LessThan => (self.load32(addr)? as i32) < v32,
            ArbitrationType::Equal => self.load32(addr)? as i32 == v32,
            ArbitrationType::Equal64 => {
                self.mem
                    .load64(addr)
                    .ok_or(KResult::INVALID_CURRENT_MEMORY)?
                    == value as u64
            }
            ArbitrationType::DecrementAndWaitIfLessThan => loop {
                let v = self.load32(addr)?;
                if (v as i32) >= v32 {
                    break false;
                }
                if self.cas(addr, v, v.wrapping_sub(1))? {
                    break true;
                }
            },
        };
        if !ok {
            return Err(KResult::INVALID_STATE);
        }
        if timeout == Timeout::Immediate {
            return Err(KResult::TIMED_OUT);
        }
        g.push(addr, self.entry(&self.addresses, cur, 0, 0));
        drop(g);
        if self.block_on(cur, timeout, || self.addresses.lock(addr).remove(addr, cur)) {
            Ok(())
        } else {
            Err(KResult::TIMED_OUT)
        }
    }

    /// `SignalToAddress`: changes the word as `ty` says, then wakes `count` waiters, all for a count of zero or less.
    pub fn signal_to_address(
        &self,
        addr: u64,
        ty: SignalType,
        value: i32,
        count: i32,
    ) -> KResultOr<()> {
        aligned(addr, 4)?;
        let mut g = self.addresses.lock(addr);
        let new = match ty {
            SignalType::Signal => None,
            SignalType::SignalAndIncrementIfEqual => Some(value.wrapping_add(1)),
            SignalType::SignalAndModifyByWaitingCountIfEqual => {
                let waiting = i64::try_from(g.count(addr)).unwrap_or(i64::MAX);
                // Mesosphere's `KAddressArbiter::SignalAndModifyByWaitingCountIfEqual`. Waking all with waiters
                // subtracts two, not one.
                Some(if count <= 0 {
                    if waiting > 0 {
                        value.wrapping_sub(2)
                    } else {
                        value.wrapping_add(1)
                    }
                } else if waiting == 0 {
                    value.wrapping_add(1)
                } else if waiting <= i64::from(count) {
                    value.wrapping_sub(1)
                } else {
                    value
                })
            }
        };
        if let Some(new) = new
            && !self.cas(addr, value as u32, new as u32)?
        {
            return Err(KResult::INVALID_STATE);
        }
        let mut woken = 0;
        while count <= 0 || woken < count {
            let Some(w) = g.pop_best(addr) else { break };
            w.thread.parker.wake(KResult::SUCCESS);
            woken += 1;
        }
        Ok(())
    }
}
