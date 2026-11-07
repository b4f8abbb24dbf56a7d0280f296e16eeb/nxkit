//! The vsync event of the game follows the ticks of the display link, once per game vsync.

#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nxkit_abi::kernel::{Kernel, ResultCode, Session, set_kernel};
use nxkit_svc_pacing::{Config, StepMode, VSYNC_EVENT, exports, init, on_display_tick, snapshot};

const HANDLE: u32 = 0x77;

#[derive(Default)]
struct Mock {
    signals: AtomicU32,
    closed: Mutex<bool>,
}

impl Kernel for Mock {
    fn create_event(&self) -> Result<(u32, u32), ResultCode> {
        Ok((1, 2))
    }
    fn signal_event(&self, handle: u32) -> Result<(), ResultCode> {
        let closed = self.closed.lock().map(|c| *c).unwrap_or(true);
        if handle != HANDLE || closed {
            return Err(0xE401);
        }
        self.signals.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn clear_event(&self, _: u32) -> Result<(), ResultCode> {
        Ok(())
    }
    fn add_session(&self, _: Arc<dyn Session>) -> Result<u32, ResultCode> {
        Ok(3)
    }
    fn create_shared_memory(&self, _: u64) -> Result<(u32, u64), ResultCode> {
        Ok((4, 0))
    }
    fn close_handle(&self, _: u32) -> Result<(), ResultCode> {
        Ok(())
    }
}

fn bind(_event: u64) -> Result<u32, u32> {
    Ok(HANDLE)
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(5), "timed out: {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_game_at_60_hz_gets_an_event_on_every_second_tick_of_120_hz() {
    let kernel = Arc::new(Mock::default());
    assert!(set_kernel(kernel.clone()));
    let config = Config {
        bind_system_event: bind,
        state_page: None,
        display_hz: 120.0,
        vsync_hz: 60.0,
        step: StepMode::Own,
        limits: None,
        display_mode: false,
    };
    assert!(init(&config).is_ok());
    assert!(init(&config).is_err());

    let Some(e) = exports().into_iter().find(|e| e.name == VSYNC_EVENT) else {
        return;
    };
    // SAFETY: the export is the `extern "C" fn(u64, u64) -> u32` of this crate.
    let call: extern "C" fn(u64, u64) -> u32 = unsafe { std::mem::transmute(e.addr) };
    assert_eq!(call(0x1000, 0), 0);
    // The same event again starts no second thread.
    assert_eq!(call(0x1000, 0), 0);
    until("the worker runs", || {
        snapshot().is_ok_and(|s| s.events == 1)
    });
    // The worker reads the tick count when it starts. Give it the time to do that before the first tick.
    thread::sleep(Duration::from_millis(50));

    for _ in 0..20 {
        assert!(on_display_tick().is_ok());
        thread::sleep(Duration::from_millis(3));
    }
    until("ten events", || kernel.signals.load(Ordering::SeqCst) >= 10);
    thread::sleep(Duration::from_millis(30));
    let n = kernel.signals.load(Ordering::SeqCst);
    assert!((10..=11).contains(&n), "{n} events for 20 ticks");
    let Ok(s) = snapshot() else {
        return;
    };
    assert_eq!(s.ticks, 20);
    assert_eq!(s.ticks_per_vsync, 2);
    assert_eq!(s.events, 1);

    // The game destroys the event. The worker ends on the next signal.
    if let Ok(mut c) = kernel.closed.lock() {
        *c = true;
    }
    for _ in 0..4 {
        assert!(on_display_tick().is_ok());
        thread::sleep(Duration::from_millis(3));
    }
    until("the worker ends", || {
        snapshot().is_ok_and(|s| s.events == 0)
    });
}
