//! WaitForAddress and SignalToAddress, and the process-wide key condition variable.

#![allow(clippy::expect_used)]
mod common;
use common::*;
use nxkit_hos::{ArbitrationType, KResult, SignalType, Timeout};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const SHORT: Timeout = Timeout::After(Duration::from_millis(30));

#[test]
fn less_than_waits_only_while_the_word_is_below_the_value() {
    let w = world();
    let a = w.addr(0);
    w.store(0, 5);
    let k = &w.kernel;
    assert_eq!(
        k.wait_for_address(&w.main, a, ArbitrationType::LessThan, 5, Timeout::Immediate),
        Err(KResult::INVALID_STATE)
    );
    assert_eq!(
        k.wait_for_address(&w.main, a, ArbitrationType::LessThan, 6, Timeout::Immediate),
        Err(KResult::TIMED_OUT)
    );
    assert_eq!(
        k.wait_for_address(&w.main, a, ArbitrationType::LessThan, 6, SHORT),
        Err(KResult::TIMED_OUT)
    );
    assert_eq!(w.load(0), 5);
}

#[test]
fn decrement_and_wait_changes_the_word_only_when_it_waits() {
    let w = world();
    let a = w.addr(0);
    let k = &w.kernel;
    w.store(0, 5);
    let ty = ArbitrationType::DecrementAndWaitIfLessThan;
    assert_eq!(
        k.wait_for_address(&w.main, a, ty, 5, SHORT),
        Err(KResult::INVALID_STATE)
    );
    assert_eq!(w.load(0), 5);
    assert_eq!(
        k.wait_for_address(&w.main, a, ty, 6, SHORT),
        Err(KResult::TIMED_OUT)
    );
    assert_eq!(w.load(0), 4);
}

#[test]
fn equal_and_equal64_compare_the_value() {
    let w = world();
    let k = &w.kernel;
    w.store(0, 7);
    let a = w.addr(0);
    assert_eq!(
        k.wait_for_address(&w.main, a, ArbitrationType::Equal, 8, SHORT),
        Err(KResult::INVALID_STATE)
    );
    assert_eq!(
        k.wait_for_address(&w.main, a, ArbitrationType::Equal, 7, SHORT),
        Err(KResult::TIMED_OUT)
    );
    w.store(0, 1);
    w.store(1, 1);
    let want = (1i64 << 32) | 1;
    assert_eq!(
        k.wait_for_address(&w.main, a, ArbitrationType::Equal64, want, SHORT),
        Err(KResult::TIMED_OUT)
    );
    assert_eq!(
        k.wait_for_address(&w.main, a, ArbitrationType::Equal64, 1, SHORT),
        Err(KResult::INVALID_STATE)
    );
}

#[test]
fn bad_addresses_and_types_are_refused() {
    let w = world();
    let k = &w.kernel;
    assert_eq!(
        k.wait_for_address(&w.main, w.addr(0) + 1, ArbitrationType::Equal, 0, SHORT),
        Err(KResult::INVALID_ADDRESS)
    );
    assert_eq!(
        k.wait_for_address(&w.main, 0x10, ArbitrationType::Equal, 0, SHORT),
        Err(KResult::INVALID_CURRENT_MEMORY)
    );
    assert!(ArbitrationType::from_raw(9).is_err());
    assert!(SignalType::from_raw(9).is_err());
}

#[test]
fn signal_wakes_the_requested_count_best_priority_first() {
    let w = world();
    let a = w.addr(0);
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hs = Vec::new();
    for prio in [40, 20, 30] {
        let k = w.kernel.clone();
        let order = order.clone();
        hs.push(w.spawn("waiter", prio, 0, move |t| {
            assert_eq!(
                k.wait_for_address(t, a, ArbitrationType::Equal, 0, Timeout::Infinite),
                Ok(())
            );
            order.lock().expect("lock").push(prio);
        }));
        let n = hs.len() as u64;
        w.wait_until("waiter parked", || w.blocked() >= n);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        w.kernel.signal_to_address(a, SignalType::Signal, 0, 1),
        Ok(())
    );
    w.wait_until("first wake", || order.lock().expect("lock").len() == 1);
    assert_eq!(*order.lock().expect("lock"), vec![20]);
    for n in 2..=3 {
        assert_eq!(
            w.kernel.signal_to_address(a, SignalType::Signal, 0, 1),
            Ok(())
        );
        w.wait_until("next wake", || order.lock().expect("lock").len() == n);
    }
    w.join(&hs);
    assert_eq!(*order.lock().expect("lock"), vec![20, 30, 40]);
}

#[test]
fn signal_and_increment_needs_the_expected_value() {
    let w = world();
    let a = w.addr(0);
    w.store(0, 3);
    let k = &w.kernel;
    assert_eq!(
        k.signal_to_address(a, SignalType::SignalAndIncrementIfEqual, 2, 1),
        Err(KResult::INVALID_STATE)
    );
    assert_eq!(w.load(0), 3);
    assert_eq!(
        k.signal_to_address(a, SignalType::SignalAndIncrementIfEqual, 3, 1),
        Ok(())
    );
    assert_eq!(w.load(0), 4);
}

fn park(w: &World, n: usize, a: u64) -> Vec<u32> {
    (0..n)
        .map(|i| {
            let k = w.kernel.clone();
            let h = w.spawn("waiter", 30, 0, move |t| {
                let _ = k.wait_for_address(t, a, ArbitrationType::Equal, 0, Timeout::Infinite);
            });
            w.wait_until("waiter parked", || w.blocked() > i as u64);
            h
        })
        .collect()
}

#[test]
fn modify_by_waiting_count_follows_the_kernel_table() {
    let k_ty = SignalType::SignalAndModifyByWaitingCountIfEqual;
    // No waiter: the word grows by one.
    let w = world();
    w.store(0, 0);
    assert_eq!(w.kernel.signal_to_address(w.addr(0), k_ty, 0, 2), Ok(()));
    assert_eq!(w.load(0), 1);
    // Two waiters, count 3: fewer waiters than the count, so minus one.
    let w = world();
    let a = w.addr(0);
    let hs = park(&w, 2, a);
    assert_eq!(w.kernel.signal_to_address(a, k_ty, 0, 3), Ok(()));
    assert_eq!(w.load(0), u32::MAX);
    w.join(&hs);
    // Two waiters, count 1: more waiters than the count, the word stays.
    let w = world();
    let a = w.addr(0);
    let hs = park(&w, 2, a);
    assert_eq!(w.kernel.signal_to_address(a, k_ty, 0, 1), Ok(()));
    assert_eq!(w.load(0), 0);
    assert_eq!(w.kernel.signal_to_address(a, k_ty, 0, 0), Ok(()));
    w.join(&hs);
    // Wake all with waiters: minus two.
    let w = world();
    let a = w.addr(0);
    let hs = park(&w, 1, a);
    assert_eq!(w.kernel.signal_to_address(a, k_ty, 0, -1), Ok(()));
    assert_eq!(w.load(0), (-2i32) as u32);
    w.join(&hs);
    // A wrong expected value changes nothing and wakes nobody.
    let w = world();
    w.store(0, 9);
    assert_eq!(
        w.kernel.signal_to_address(w.addr(0), k_ty, 0, 1),
        Err(KResult::INVALID_STATE)
    );
}

fn mutex_and_cv(w: &World) -> (Arc<GuestMutex>, Arc<GuestCondvar>) {
    let m = Arc::new(GuestMutex {
        kernel: w.kernel.clone(),
        mem: w.mem.clone(),
        addr: w.addr(0),
    });
    (m, Arc::new(GuestCondvar { key: w.addr(1) }))
}

#[test]
fn condvar_signal_wakes_as_many_threads_as_asked() {
    let w = world();
    let (m, cv) = mutex_and_cv(&w);
    let woken = Arc::new(AtomicU32::new(0));
    let mut hs = Vec::new();
    for _ in 0..3 {
        let (m, cv, woken) = (m.clone(), cv.clone(), woken.clone());
        hs.push(w.spawn("cv", 30, 0, move |t| {
            m.lock(t);
            assert_eq!(cv.wait(&m, t, Timeout::Infinite), Ok(()));
            woken.fetch_add(1, Ordering::SeqCst);
            m.unlock(t);
        }));
    }
    w.wait_until("all wait", || w.load(1) != 0 && w.blocked() >= 3);
    cv.signal(&m, 1);
    w.wait_until("one wakes", || woken.load(Ordering::SeqCst) == 1);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    assert_ne!(w.load(1), 0, "the key stays set while threads wait");
    cv.signal(&m, 2);
    w.join(&hs);
    assert_eq!(woken.load(Ordering::SeqCst), 3);
    assert_eq!(w.load(1), 0, "the key is cleared when nobody waits");
}

#[test]
fn condvar_wait_times_out_without_the_lock_and_unlocks_while_waiting() {
    let w = world();
    let (m, cv) = mutex_and_cv(&w);
    m.lock(&w.main);
    assert_eq!(cv.wait(&m, &w.main, SHORT), Err(KResult::TIMED_OUT));
    assert_eq!(
        w.load(0) & !nxkit_hos::WAITERS_BIT,
        w.main.handle(),
        "the test relocked after the timeout"
    );
    m.unlock(&w.main);
    assert_eq!(w.load(0), 0);
}

#[test]
fn condvar_signal_with_count_zero_wakes_all() {
    let w = world();
    let (m, cv) = mutex_and_cv(&w);
    let hs: Vec<u32> = (0..4)
        .map(|_| {
            let (m, cv) = (m.clone(), cv.clone());
            w.spawn("cv", 30, 0, move |t| {
                m.lock(t);
                let _ = cv.wait(&m, t, Timeout::Infinite);
                m.unlock(t);
            })
        })
        .collect();
    w.wait_until("all wait", || w.blocked() >= 4 && w.load(0) == 0);
    cv.signal(&m, -1);
    w.join(&hs);
}

#[test]
fn producer_consumer_stress_loses_no_wakeup() {
    let w = world();
    let (m, cv) = mutex_and_cv(&w);
    let items = w.addr(2);
    let consumed = Arc::new(AtomicU32::new(0));
    const PRODUCERS: u32 = 4;
    const CONSUMERS: u32 = 4;
    const EACH: u32 = 500;
    let mut hs = Vec::new();
    for c in 0..CONSUMERS {
        let (m, cv, mem, consumed) = (m.clone(), cv.clone(), w.mem.clone(), consumed.clone());
        hs.push(
            w.spawn("consumer", 20 + c as i32, (c % 3) as i32, move |t| {
                for _ in 0..(PRODUCERS * EACH / CONSUMERS) {
                    m.lock(t);
                    while nxkit_hos::GuestMemory::load32(&*mem, items) == Some(0) {
                        assert_eq!(cv.wait(&m, t, Timeout::Infinite), Ok(()));
                    }
                    let n = nxkit_hos::GuestMemory::load32(&*mem, items).unwrap_or(0);
                    nxkit_hos::GuestMemory::store32(&*mem, items, n - 1);
                    consumed.fetch_add(1, Ordering::SeqCst);
                    m.unlock(t);
                }
            }),
        );
    }
    for p in 0..PRODUCERS {
        let (m, cv, mem) = (m.clone(), cv.clone(), w.mem.clone());
        hs.push(
            w.spawn("producer", 30 + p as i32, (p % 3) as i32, move |t| {
                for _ in 0..EACH {
                    m.lock(t);
                    let n = nxkit_hos::GuestMemory::load32(&*mem, items).unwrap_or(0);
                    nxkit_hos::GuestMemory::store32(&*mem, items, n + 1);
                    m.unlock(t);
                    cv.signal(&m, 1);
                }
            }),
        );
    }
    w.join(&hs);
    assert_eq!(consumed.load(Ordering::SeqCst), PRODUCERS * EACH);
    assert_eq!(w.load(2), 0);
}
