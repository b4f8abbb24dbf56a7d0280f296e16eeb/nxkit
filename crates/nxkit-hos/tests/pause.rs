//! `SetThreadActivity` and `GetThreadContext3`: the results of Horizon, a thread paused in a svc, and a host thread
//! stopped in "guest" code with `thread_suspend`.

#![allow(clippy::expect_used)]
mod common;
use common::*;
use nxkit_hos::{
    CURRENT_THREAD, Config, KResult, ThreadContext, ThreadControl, ThreadSpec, current_thread,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

const RUNNABLE: u32 = 0;
const PAUSED: u32 = 1;

/// A gate frame reader that makes the registers from the frame address, and a guest-code test that says no for the
/// first `refuse` calls.
struct Fake {
    refuse: usize,
    asked: AtomicUsize,
}

impl ThreadControl for Fake {
    fn gate_context(&self, frame: u64) -> ThreadContext {
        let mut c = ThreadContext::default();
        for (i, x) in c.x.iter_mut().enumerate() {
            *x = frame + i as u64;
        }
        (c.fp, c.lr, c.sp, c.pc) = (frame + 29, frame + 30, frame + 31, frame + 32);
        c.pstate = 0x6000_03c0;
        c
    }

    fn is_guest_code(&self, _pc: u64) -> bool {
        self.asked.fetch_add(1, Ordering::SeqCst) >= self.refuse
    }
}

fn paused_world(refuse: usize) -> (World, Arc<Fake>) {
    let fake = Arc::new(Fake {
        refuse,
        asked: AtomicUsize::new(0),
    });
    let w = world_with(Config {
        threads: Some(fake.clone()),
        ..Config::default()
    });
    (w, fake)
}

#[test]
fn the_results_follow_horizon() {
    let (w, _) = paused_world(0);
    let k = &w.kernel;
    let main = &w.main;
    assert_eq!(
        k.set_thread_activity(main, 0x1234, 2),
        Err(KResult::INVALID_ENUM_VALUE)
    );
    assert_eq!(
        k.set_thread_activity(main, 0x1234, PAUSED),
        Err(KResult::INVALID_HANDLE)
    );
    let (event, _) = k.create_event().expect("event");
    assert_eq!(
        k.set_thread_activity(main, event, PAUSED),
        Err(KResult::INVALID_HANDLE)
    );
    assert_eq!(
        k.set_thread_activity(main, CURRENT_THREAD, PAUSED),
        Err(KResult::BUSY)
    );
    assert_eq!(
        k.set_thread_activity(main, main.handle(), PAUSED),
        Err(KResult::BUSY)
    );
    assert_eq!(
        k.get_thread_context3(main, main.handle()),
        Err(KResult::BUSY)
    );
    let created = k
        .create_thread(ThreadSpec {
            name: "created".into(),
            entry: 0,
            priority: 30,
            core: 0,
            stack_size: 0,
            body: Box::new(|| {}),
        })
        .expect("create");
    assert_eq!(
        k.set_thread_activity(main, created, PAUSED),
        Err(KResult::INVALID_STATE)
    );
    let ended = w.spawn("ended", 30, 0, |_| {});
    w.join(&[ended]);
    assert_eq!(
        k.set_thread_activity(main, ended, PAUSED),
        Err(KResult::INVALID_STATE)
    );
}

#[test]
fn the_svcs_are_not_answered_without_the_thread_control_of_the_host() {
    let w = world();
    let mut regs = [0u64; 8];
    assert!(!w.kernel.dispatch_svc(&w.main, 0x32, &mut regs));
    assert!(!w.kernel.dispatch_svc(&w.main, 0x33, &mut regs));
}

/// A thread that enters a svc with the frame `frame`, says so, and leaves the svc when `finish` is set.
fn svc_thread(w: &World, frame: u64, finish: Arc<AtomicBool>, left: Arc<AtomicBool>) -> u32 {
    let (k, inside) = (w.kernel.clone(), Arc::new(AtomicBool::new(false)));
    let inside2 = inside.clone();
    let h = w.spawn("in-svc", 30, 0, move |t| {
        k.gate_enter(t, frame);
        inside2.store(true, Ordering::SeqCst);
        while !finish.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        k.gate_leave(t);
        left.store(true, Ordering::SeqCst);
    });
    w.wait_until("the thread is in the svc", || inside.load(Ordering::SeqCst));
    h
}

#[test]
fn a_thread_in_a_svc_is_paused_at_once_and_waits_at_its_end_until_resumed() {
    let (w, fake) = paused_world(0);
    let (finish, left) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let h = svc_thread(&w, 0x8000, finish.clone(), left.clone());
    let (k, main) = (&w.kernel, &w.main);
    assert_eq!(
        k.get_thread_context3(main, h),
        Err(KResult::INVALID_STATE),
        "not paused"
    );
    assert_eq!(
        k.set_thread_activity(main, h, RUNNABLE),
        Err(KResult::INVALID_STATE)
    );
    assert_eq!(k.set_thread_activity(main, h, PAUSED), Ok(()));
    assert_eq!(fake.asked.load(Ordering::SeqCst), 0, "no thread_suspend");
    assert_eq!(
        k.set_thread_activity(main, h, PAUSED),
        Err(KResult::INVALID_STATE)
    );

    finish.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !left.load(Ordering::SeqCst),
        "a paused thread does not leave the svc"
    );

    let c = k.get_thread_context3(main, h).expect("context");
    assert_eq!(
        c.x[..19],
        [0; 19],
        "x0 to x18 of a thread in a svc are zero"
    );
    assert_eq!(c.x[19], 0x8000 + 19);
    assert_eq!(c.x[28], 0x8000 + 28);
    assert_eq!(
        (c.fp, c.lr, c.sp, c.pc),
        (0x8000 + 29, 0x8000 + 30, 0x8000 + 31, 0x8000 + 32)
    );
    assert_eq!(c.pstate, 0x6000_0000, "only the condition flags");

    assert_eq!(k.set_thread_activity(main, h, RUNNABLE), Ok(()));
    w.wait_until("the resumed thread leaves the svc", || {
        left.load(Ordering::SeqCst)
    });
    w.join(&[h]);
}

#[test]
fn the_svc_writes_the_context_in_the_horizon_layout() {
    let (w, _) = paused_world(0);
    let (finish, left) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let h = svc_thread(&w, 0x9000, finish.clone(), left);
    let mut regs = [u64::from(h), u64::from(PAUSED), 0, 0, 0, 0, 0, 0];
    assert!(w.kernel.dispatch_svc(&w.main, 0x32, &mut regs));
    assert_eq!(regs[0], 0);
    let out = w.addr(0);
    let mut regs = [out, u64::from(h), 0, 0, 0, 0, 0, 0];
    assert!(w.kernel.dispatch_svc(&w.main, 0x33, &mut regs));
    assert_eq!(regs[0], 0);
    let word = |byte: u64| {
        let lo = u64::from(w.load((byte / 4) as usize));
        lo | u64::from(w.load((byte / 4) as usize + 1)) << 32
    };
    assert_eq!(word(0), 0);
    assert_eq!(word(19 * 8), 0x9000 + 19);
    assert_eq!(word(0xF8), 0x9000 + 31, "sp at 0xF8");
    assert_eq!(word(0x108), 0x6000_0000, "pstate at 0x108");
    let mut regs = [w.addr(250), u64::from(h), 0, 0, 0, 0, 0, 0];
    assert!(w.kernel.dispatch_svc(&w.main, 0x33, &mut regs));
    assert_eq!(regs[0], u64::from(KResult::INVALID_POINTER.raw()));
    let mut regs = [u64::from(h), u64::from(RUNNABLE), 0, 0, 0, 0, 0, 0];
    assert!(w.kernel.dispatch_svc(&w.main, 0x32, &mut regs));
    assert_eq!(regs[0], 0);
    finish.store(true, Ordering::SeqCst);
    w.join(&[h]);
}

/// A thread that spins outside any svc and counts its turns. It reports the address of a local on its stack.
fn spinner(w: &World, stop: Arc<AtomicBool>, turns: Arc<AtomicU64>, local: Arc<AtomicU64>) -> u32 {
    let counter = turns.clone();
    let h = w.spawn("spinner", 30, 0, move |_| {
        let mark = 0u64;
        local.store(std::ptr::addr_of!(mark) as u64, Ordering::SeqCst);
        while !stop.load(Ordering::SeqCst) {
            counter.fetch_add(1, Ordering::SeqCst);
            std::hint::spin_loop();
        }
        std::hint::black_box(&mark);
    });
    w.wait_until("the spinner runs", || turns.load(Ordering::SeqCst) > 1000);
    h
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_thread_in_guest_code_is_suspended_and_its_stack_pointer_is_read() {
    let (w, fake) = paused_world(3);
    let (stop, turns, local) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
    );
    let h = spinner(&w, stop.clone(), turns.clone(), local.clone());
    let (k, main) = (&w.kernel, &w.main);
    assert_eq!(k.set_thread_activity(main, h, PAUSED), Ok(()));
    assert_eq!(
        fake.asked.load(Ordering::SeqCst),
        4,
        "three tries in host code, the fourth in guest code"
    );
    let n = turns.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        turns.load(Ordering::SeqCst),
        n,
        "a suspended thread runs no more"
    );

    let c = k.get_thread_context3(main, h).expect("context");
    let mark = local.load(Ordering::SeqCst);
    assert!(
        c.sp <= mark && mark - c.sp < 64 << 10,
        "sp {:#x} is below the local at {mark:#x} on the same stack",
        c.sp
    );
    assert_ne!(c.pc, 0);
    assert_eq!(c.pstate & !0xF000_0000, 0);

    assert_eq!(k.set_thread_activity(main, h, RUNNABLE), Ok(()));
    w.wait_until("the resumed thread runs", || {
        turns.load(Ordering::SeqCst) > n + 1000
    });
    stop.store(true, Ordering::SeqCst);
    w.join(&[h]);
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_thread_that_enters_a_svc_while_the_pause_waits_is_paused_there() {
    let (w, _) = paused_world(usize::MAX);
    let (go, inside) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (go2, inside2, left) = (go.clone(), inside.clone(), Arc::new(AtomicBool::new(false)));
    let (k, left2) = (w.kernel.clone(), left.clone());
    let h = w.spawn("to-svc", 30, 0, move |t| {
        inside2.store(true, Ordering::SeqCst);
        while !go2.load(Ordering::SeqCst) {
            std::hint::spin_loop();
        }
        k.gate_enter(t, 0xA000);
        k.gate_leave(t);
        left2.store(true, Ordering::SeqCst);
    });
    w.wait_until("the thread runs", || inside.load(Ordering::SeqCst));
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        go.store(true, Ordering::SeqCst);
    });
    let (k, main) = (&w.kernel, &w.main);
    assert_eq!(k.set_thread_activity(main, h, PAUSED), Ok(()));
    let c = k.get_thread_context3(main, h).expect("context");
    assert_eq!(c.sp, 0xA000 + 31, "the registers of the gate frame");
    std::thread::sleep(Duration::from_millis(20));
    assert!(!left.load(Ordering::SeqCst));
    assert_eq!(k.set_thread_activity(main, h, RUNNABLE), Ok(()));
    w.join(&[h]);
    assert!(left.load(Ordering::SeqCst));
}

#[test]
fn a_thread_that_exits_in_a_svc_while_paused_does_not_wait_and_reports_no_registers() {
    let (w, _) = paused_world(0);
    let (k, inside, go) = (
        w.kernel.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (inside2, go2, left) = (inside.clone(), go.clone(), Arc::new(AtomicBool::new(false)));
    let left2 = left.clone();
    let h = w.spawn("exits", 30, 0, move |t| {
        k.gate_enter(t, 0xB000);
        inside2.store(true, Ordering::SeqCst);
        while !go2.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        k.exit_thread(t);
        k.gate_leave(t);
        left2.store(current_thread().is_some(), Ordering::SeqCst);
    });
    w.wait_until("the thread is in the svc", || inside.load(Ordering::SeqCst));
    let (k, main) = (&w.kernel, &w.main);
    assert_eq!(k.set_thread_activity(main, h, PAUSED), Ok(()));
    go.store(true, Ordering::SeqCst);
    w.wait_until("the exiting thread leaves the svc", || {
        left.load(Ordering::SeqCst)
    });
    w.join(&[h]);
    assert_eq!(k.get_thread_context3(main, h), Ok(ThreadContext::default()));
    assert_eq!(
        k.set_thread_activity(main, h, RUNNABLE),
        Err(KResult::INVALID_STATE)
    );
    assert_eq!(k.get_thread_context3(main, h), Err(KResult::INVALID_STATE));
}
