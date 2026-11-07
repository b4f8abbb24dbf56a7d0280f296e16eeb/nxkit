//! ArbitrateLock and ArbitrateUnlock: ownership handoff, priority order, and the errors.

#![allow(clippy::expect_used)]
mod common;
use common::*;
use nxkit_hos::{GuestMemory, KResult, Timeout, WAITERS_BIT};
use std::sync::{Arc, Mutex};

#[test]
fn unlock_hands_the_lock_to_the_highest_priority_waiter_first() {
    let w = world();
    let lock = w.addr(0);
    let owner = w.main.handle();
    w.store(0, owner | WAITERS_BIT);
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for prio in [50, 40, 45, 35] {
        let (k, order) = (w.kernel.clone(), order.clone());
        handles.push(w.spawn("waiter", prio, 1, move |t| {
            assert_eq!(k.arbitrate_lock(t, owner, lock, t.handle()), Ok(()));
            order.lock().map(|mut o| o.push(prio)).ok();
            assert_eq!(k.arbitrate_unlock(lock), Ok(()));
        }));
    }
    w.wait_until("four lock waits", || w.contended() == 4);
    assert_eq!(w.kernel.arbitrate_unlock(lock), Ok(()));
    w.join(&handles);
    assert_eq!(
        order.lock().map(|o| o.clone()).unwrap_or_default(),
        [35, 40, 45, 50]
    );
    assert_eq!(w.load(0), 0, "the last unlock frees the lock");
}

#[test]
fn the_word_names_the_new_owner_and_keeps_the_waiter_bit_while_threads_wait() {
    let w = world();
    let lock = w.addr(0);
    let owner = w.main.handle();
    w.store(0, owner | WAITERS_BIT);
    let mut hs = Vec::new();
    let mut tags = Vec::new();
    for prio in [40, 41] {
        let k = w.kernel.clone();
        let h = w.spawn("waiter", prio, 1, move |t| {
            let _ = k.arbitrate_lock(t, owner, lock, t.handle());
        });
        tags.push(h);
        hs.push(h);
        let want = tags.len() as u64;
        w.wait_until("lock wait", || w.contended() == want);
    }
    assert_eq!(w.kernel.arbitrate_unlock(lock), Ok(()));
    assert_eq!(
        w.load(0),
        tags[0] | WAITERS_BIT,
        "first comer at equal priority, one more waits"
    );
    assert_eq!(w.kernel.arbitrate_unlock(lock), Ok(()));
    assert_eq!(w.load(0), tags[1], "no waiter is left");
    w.join(&hs);
}

#[test]
fn equal_priorities_are_served_in_the_order_they_arrived() {
    let w = world();
    let lock = w.addr(0);
    let owner = w.main.handle();
    w.store(0, owner | WAITERS_BIT);
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut hs = Vec::new();
    for i in 0..5u64 {
        let (k, order) = (w.kernel.clone(), order.clone());
        hs.push(w.spawn("waiter", 44, 1, move |t| {
            let _ = k.arbitrate_lock(t, owner, lock, t.handle());
            order.lock().map(|mut o| o.push(i)).ok();
            let _ = k.arbitrate_unlock(lock);
        }));
        w.wait_until("lock wait", || w.contended() == i + 1);
    }
    assert_eq!(w.kernel.arbitrate_unlock(lock), Ok(()));
    w.join(&hs);
    assert_eq!(
        order.lock().map(|o| o.clone()).unwrap_or_default(),
        [0, 1, 2, 3, 4]
    );
}

#[test]
fn a_word_that_changed_returns_at_once_and_errors_are_the_kernels() {
    let w = world();
    let (lock, me) = (w.addr(0), w.main.handle());
    w.store(0, 0);
    assert_eq!(
        w.kernel.arbitrate_lock(&w.main, me, lock, me),
        Ok(()),
        "the owner released already"
    );
    assert_eq!(w.contended(), 0);
    assert_eq!(
        w.kernel.arbitrate_lock(&w.main, 0x1234, lock, me),
        Err(KResult::INVALID_HANDLE)
    );
    assert_eq!(
        w.kernel.arbitrate_lock(&w.main, me, lock + 2, me),
        Err(KResult::INVALID_ADDRESS)
    );
    assert_eq!(
        w.kernel.arbitrate_lock(&w.main, me, 0x40, me),
        Err(KResult::INVALID_CURRENT_MEMORY)
    );
    assert_eq!(
        w.kernel.arbitrate_unlock(lock + 1),
        Err(KResult::INVALID_ADDRESS)
    );
    assert_eq!(
        w.kernel.arbitrate_unlock(0x40),
        Err(KResult::INVALID_CURRENT_MEMORY)
    );
}

#[test]
fn many_threads_share_one_guest_mutex_without_a_lost_update() {
    let w = world();
    let m = Arc::new(GuestMutex {
        kernel: w.kernel.clone(),
        mem: w.mem.clone(),
        addr: w.addr(0),
    });
    let counter = w.addr(1);
    let (threads, rounds) = (16u32, 3000u32);
    let mut hs = Vec::new();
    for i in 0..threads {
        let (m, mem) = (m.clone(), w.mem.clone());
        hs.push(
            w.spawn("inc", 30 + (i % 5) as i32, (i % 3) as i32, move |t| {
                for _ in 0..rounds {
                    m.lock(t);
                    let v = mem.load32(counter).unwrap_or(0);
                    std::hint::spin_loop();
                    mem.store32(counter, v + 1);
                    m.unlock(t);
                }
            }),
        );
    }
    w.join(&hs);
    assert_eq!(w.load(1), threads * rounds);
    assert_eq!(w.load(0), 0);
    let stats = w.kernel.stats();
    assert!(
        stats.iter().map(|s| s.contended_locks).sum::<u64>() > 0,
        "the run must have contended"
    );
    let _ = Timeout::Infinite;
}
