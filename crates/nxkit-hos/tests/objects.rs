//! Handles, threads, events, WaitSynchronization, the core rules, roles and the svc dispatch.

#![allow(clippy::expect_used)]
mod common;
use common::*;
use nxkit_hos::{Config, KResult, NameMatch, Qos, RoleRule, RoleTable, ThreadRole, Timeout, sys};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const SHORT: Timeout = Timeout::After(Duration::from_millis(30));

#[test]
fn wait_synchronization_returns_the_index_of_the_signalled_event() {
    let w = world();
    let (w1, r1) = w.kernel.create_event().expect("event");
    let (w2, r2) = w.kernel.create_event().expect("event");
    assert_eq!(
        w.kernel.wait_synchronization(&w.main, &[r1, r2], SHORT),
        Err(KResult::TIMED_OUT)
    );
    let k = w.kernel.clone();
    let h = w.spawn("signaller", 30, 0, move |_| {
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(k.signal_event(w2), Ok(()));
    });
    assert_eq!(
        w.kernel
            .wait_synchronization(&w.main, &[r1, r2], Timeout::Infinite),
        Ok(1)
    );
    w.join(&[h]);
    // Events stay signalled until cleared.
    assert_eq!(
        w.kernel
            .wait_synchronization(&w.main, &[r1, r2], Timeout::Immediate),
        Ok(1)
    );
    assert_eq!(w.kernel.clear_event(w2), Ok(()));
    assert_eq!(w.kernel.is_signaled(r2), Ok(false));
    assert_eq!(w.kernel.signal_event(w1), Ok(()));
    assert_eq!(
        w.kernel
            .wait_synchronization(&w.main, &[r1, r2], Timeout::Immediate),
        Ok(0)
    );
    assert_eq!(w.kernel.reset_signal(r1), Ok(()));
    assert_eq!(w.kernel.reset_signal(r1), Err(KResult::INVALID_STATE));
}

#[test]
fn wait_synchronization_checks_its_arguments() {
    let w = world();
    assert_eq!(
        w.kernel.wait_synchronization(&w.main, &[0x1234], SHORT),
        Err(KResult::INVALID_HANDLE)
    );
    let (wr, _) = w.kernel.create_event().expect("event");
    assert_eq!(
        w.kernel.wait_synchronization(&w.main, &[wr], SHORT),
        Err(KResult::INVALID_HANDLE)
    );
    let many = vec![wr; nxkit_hos::MAX_WAIT_HANDLES + 1];
    assert_eq!(
        w.kernel.wait_synchronization(&w.main, &many, SHORT),
        Err(KResult::OUT_OF_RANGE)
    );
    assert_eq!(w.kernel.close_handle(wr), Ok(()));
    assert_eq!(w.kernel.close_handle(wr), Err(KResult::INVALID_HANDLE));
}

#[test]
fn cancel_synchronization_ends_the_wait_with_cancelled() {
    let w = world();
    let (_, r) = w.kernel.create_event().expect("event");
    let k = w.kernel.clone();
    let result = Arc::new(AtomicU32::new(0));
    let res = result.clone();
    let h = w.spawn("waiter", 30, 0, move |t| {
        let r = k.wait_synchronization(t, &[r], Timeout::Infinite);
        res.store(r.err().map_or(1, |e| e.raw()), Ordering::SeqCst);
    });
    w.wait_until("parked", || w.blocked() >= 1);
    assert_eq!(w.kernel.cancel_synchronization(h), Ok(()));
    w.join(&[h]);
    assert_eq!(result.load(Ordering::SeqCst), KResult::CANCELLED.raw());
}

/// The SDK's `MultiWait` cancels the wait of its thread right after that thread let go of the list lock, so the cancel
/// often comes while the thread enters `WaitSynchronization`. A cancel in that moment must still end the wait.
#[test]
fn a_cancel_while_the_wait_starts_is_not_lost() {
    const ROUNDS: u32 = 3000;
    let w = world();
    let (_, r) = w.kernel.create_event().expect("event");
    let k = w.kernel.clone();
    let armed = Arc::new(AtomicU32::new(0));
    let done = Arc::new(AtomicU32::new(0));
    let lost = Arc::new(AtomicU32::new(0));
    let (a, d, l) = (armed.clone(), done.clone(), lost.clone());
    let h = w.spawn("waiter", 30, 0, move |t| {
        for round in 1..=ROUNDS {
            a.store(round, Ordering::SeqCst);
            let t0 = Instant::now();
            let res = k.wait_synchronization(t, &[r], Timeout::After(Duration::from_secs(1)));
            if res != Err(KResult::CANCELLED) || t0.elapsed() >= Duration::from_secs(1) {
                l.fetch_add(1, Ordering::SeqCst);
            }
            d.store(round, Ordering::SeqCst);
        }
    });
    for round in 1..=ROUNDS {
        while armed.load(Ordering::SeqCst) < round {
            std::hint::spin_loop();
        }
        assert_eq!(w.kernel.cancel_synchronization(h), Ok(()));
        while done.load(Ordering::SeqCst) < round {
            std::hint::spin_loop();
        }
    }
    w.join(&[h]);
    assert_eq!(
        lost.load(Ordering::SeqCst),
        0,
        "cancels that did not end the wait"
    );
}

#[test]
fn a_thread_handle_is_signalled_when_the_thread_ends() {
    let w = world();
    let gate = Arc::new(AtomicBool::new(false));
    let g = gate.clone();
    let h = w.spawn("short", 30, 0, move |_| {
        while !g.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    assert_eq!(
        w.kernel.wait_synchronization(&w.main, &[h], SHORT),
        Err(KResult::TIMED_OUT)
    );
    gate.store(true, Ordering::SeqCst);
    assert_eq!(
        w.kernel
            .wait_synchronization(&w.main, &[h], Timeout::Infinite),
        Ok(0)
    );
}

#[test]
fn thread_priority_and_core_mask_are_validated() {
    let w = world();
    let k = &w.kernel;
    let me = w.main.handle();
    assert_eq!(k.get_thread_priority(&w.main, me), Ok(30));
    assert_eq!(
        k.set_thread_priority(&w.main, me, 70),
        Err(KResult::INVALID_PRIORITY)
    );
    assert_eq!(k.set_thread_priority(&w.main, me, 10), Ok(()));
    assert_eq!(
        k.get_thread_priority(&w.main, nxkit_hos::CURRENT_THREAD),
        Ok(10)
    );
    assert_eq!(
        k.set_thread_core_mask(&w.main, me, 5, 1),
        Err(KResult::INVALID_CORE_ID)
    );
    assert_eq!(
        k.set_thread_core_mask(&w.main, me, 1, 0b100),
        Err(KResult::INVALID_COMBINATION)
    );
    assert_eq!(k.set_thread_core_mask(&w.main, me, 2, 0b110), Ok(()));
    assert_eq!(k.get_thread_core_mask(&w.main, me), Ok((2, 0b110)));
    assert_eq!(
        k.get_thread_priority(&w.main, 0x77),
        Err(KResult::INVALID_HANDLE)
    );
}

#[test]
fn the_processor_number_is_the_pinned_core_on_all_six_cores() {
    let w = world_with(Config {
        core_mask: 0b11_1111,
        ..Config::default()
    });
    let k = &w.kernel;
    let me = w.main.handle();
    for core in 0..6 {
        assert_eq!(k.set_thread_core_mask(&w.main, me, -1, 1 << core), Ok(()));
        assert_eq!(k.current_processor_number(&w.main), core);
    }
    assert_eq!(k.set_thread_core_mask(&w.main, me, 5, 0b11_0000), Ok(()));
    assert_eq!(k.current_processor_number(&w.main), 5);
}

#[test]
fn sleep_waits_and_yield_returns() {
    let w = world();
    let t0 = std::time::Instant::now();
    w.kernel.sleep_thread(&w.main, 20_000_000);
    assert!(t0.elapsed() >= Duration::from_millis(19));
    w.kernel.sleep_thread(&w.main, 0);
    w.kernel.sleep_thread(&w.main, -1);
}

#[test]
fn counters_record_waits_timeouts_and_contention() {
    let w = world();
    let (_, r) = w.kernel.create_event().expect("event");
    let _ = w.kernel.wait_synchronization(&w.main, &[r], SHORT);
    let rows = w.kernel.stats();
    let me = rows
        .iter()
        .find(|s| s.thread_id == w.main.id())
        .expect("row");
    assert_eq!((me.waits, me.timeouts), (1, 1));
    assert!(me.wait_ns >= 25_000_000);
    assert!(w.kernel.dump_stats().contains("main"));
}

fn role_config() -> Config {
    Config {
        roles: RoleTable::new(&[
            RoleRule::named(NameMatch::Prefix("Render"), ThreadRole::Render),
            RoleRule::named(NameMatch::Prefix("Load"), ThreadRole::Loading),
        ]),
        ..Config::default()
    }
}

#[test]
fn threads_get_the_role_of_the_title_table_and_unknown_is_logged_once() {
    static LOGS: AtomicU32 = AtomicU32::new(0);
    let mut config = role_config();
    config.log = |_| {
        LOGS.fetch_add(1, Ordering::SeqCst);
    };
    config.apply_qos = false;
    let w = world_with(config);
    let roles = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hs = Vec::new();
    for name in ["RenderMain", "Loader0", "mystery", "other"] {
        let roles = roles.clone();
        hs.push(w.spawn(name, 30, 0, move |t| {
            roles.lock().expect("lock").push((t.name(), t.role()))
        }));
    }
    w.join(&hs);
    let mut got = roles.lock().expect("lock").clone();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("Loader0".to_string(), ThreadRole::Loading),
            ("RenderMain".to_string(), ThreadRole::Render),
            ("mystery".to_string(), ThreadRole::Unknown),
            ("other".to_string(), ThreadRole::Unknown),
        ]
    );
    let unknown_logs = LOGS.load(Ordering::SeqCst);
    assert!(
        unknown_logs <= 1,
        "Unknown is reported once, saw {unknown_logs}"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn the_host_thread_runs_at_the_qos_of_its_role_not_its_priority() {
    let w = world_with(role_config());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hs = Vec::new();
    // The loader has the highest Switch priority and still lands on the utility class.
    for (name, prio) in [("RenderMain", 59), ("Loader0", 16)] {
        let (seen, k) = (seen.clone(), w.kernel.clone());
        hs.push(w.spawn(name, prio, 0, move |t| {
            k.sleep_thread(t, 0);
            seen.lock()
                .expect("lock")
                .push((t.name(), sys::current_qos()));
        }));
    }
    w.join(&hs);
    let mut got = seen.lock().expect("lock").clone();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got[0], ("Loader0".to_string(), Some(Qos::Utility)));
    assert_eq!(
        got[1],
        ("RenderMain".to_string(), Some(Qos::UserInteractive))
    );
}

#[test]
fn core_rules_are_off_by_default_and_hold_back_lower_priorities_when_on() {
    for on in [false, true] {
        let w = world_with(Config {
            core_rules: on,
            apply_qos: false,
            ..Config::default()
        });
        let (_, r) = w.kernel.create_event().expect("event");
        let hi_running = Arc::new(AtomicBool::new(false));
        let lo_ran_early = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let (flag, rel, k) = (hi_running.clone(), release.clone(), w.kernel.clone());
        let hi = w.spawn("hi", 10, 1, move |t| {
            flag.store(true, Ordering::SeqCst);
            while !rel.load(Ordering::SeqCst) {
                k.sleep_thread(t, 0);
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        w.wait_until("hi runs", || hi_running.load(Ordering::SeqCst));
        let (early, hi_flag, k) = (lo_ran_early.clone(), release.clone(), w.kernel.clone());
        let lo = w.spawn("lo", 40, 1, move |t| {
            // Reaches its first svc while "hi" is runnable on the same core.
            let _ = k.wait_synchronization(t, &[r], Timeout::Immediate);
            if !hi_flag.load(Ordering::SeqCst) {
                early.store(true, Ordering::SeqCst);
            }
        });
        std::thread::sleep(Duration::from_millis(50));
        release.store(true, Ordering::SeqCst);
        w.join(&[hi, lo]);
        assert_eq!(lo_ran_early.load(Ordering::SeqCst), !on, "core rules {on}");
    }
}

#[test]
fn dispatch_svc_answers_the_wait_and_event_calls() {
    let w = world();
    let (wr, rd) = w.kernel.create_event().expect("event");
    let mut regs = [0u64; 8];
    regs[0] = u64::from(wr);
    assert!(w.kernel.dispatch_svc(&w.main, 0x11, &mut regs));
    assert_eq!(regs[0], 0);
    assert_eq!(w.kernel.is_signaled(rd), Ok(true));
    let mut regs = [0u64; 8];
    regs[0] = 0x1_0000;
    assert!(!w.kernel.dispatch_svc(&w.main, 0x7777, &mut regs));
}

#[test]
fn a_higher_class_waiter_can_block_on_a_lower_class_owner() {
    let w = world_with(role_config());
    let m = Arc::new(GuestMutex {
        kernel: w.kernel.clone(),
        mem: w.mem.clone(),
        addr: w.addr(0),
    });
    let held = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (m1, held1, rel1) = (m.clone(), held.clone(), release.clone());
    let owner = w.spawn("Loader0", 40, 0, move |t| {
        m1.lock(t);
        held1.store(true, Ordering::SeqCst);
        while !rel1.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        m1.unlock(t);
    });
    w.wait_until("owner holds", || held.load(Ordering::SeqCst));
    let m2 = m.clone();
    let waiter = w.spawn("RenderMain", 20, 1, move |t| {
        m2.lock(t);
        m2.unlock(t);
    });
    w.wait_until("waiter blocked", || w.blocked() >= 1);
    release.store(true, Ordering::SeqCst);
    w.join(&[owner, waiter]);
    assert_eq!(w.load(0), 0);
}
