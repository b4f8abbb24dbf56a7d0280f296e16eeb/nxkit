//! Handles, threads, events, WaitSynchronization, the core rules, roles and the svc dispatch.

#![allow(clippy::expect_used)]
mod common;
use common::*;
use nxkit_hos::{Config, KResult, NameMatch, Qos, RoleRule, RoleTable, ThreadRole, Timeout, sys};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

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
