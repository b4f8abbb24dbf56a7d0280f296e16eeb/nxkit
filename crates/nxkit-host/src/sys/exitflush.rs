//! The write of the runtime pipeline cache and the last send of the cloud saves when the process ends.
//!
//! The host ends with `std::process::exit` (the main thread, the watchdog of `stop.rs`, a guest `ExitProcess`), which
//! runs the C library's `atexit` handlers but no destructor of the GPU backend. One handler writes what the run has
//! compiled (`nxkit_gpu::flush_runtime_cache`), so the next start finds it, and sends the committed save that the
//! cloud does not have yet (`savesync::at_exit`).
//!
//! `SIGTERM`, `SIGINT` and `SIGHUP` (a `kill`, Ctrl-C, a closed terminal) would end the process at once, without the
//! handler. They ask for a stop instead (`stop::request`, as a closed window does): the window loop ends, the process
//! exits and the handler runs once. A second signal of the same kind still ends the process at once.

use std::ffi::c_int;
use std::sync::Once;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> c_int;
    fn signal(sig: c_int, handler: usize) -> usize;
}

/// The signals that ask for a stop: `SIGHUP`, `SIGINT` and `SIGTERM`.
const STOP_SIGNALS: [(c_int, &str); 3] = [(1, "SIGHUP"), (2, "SIGINT"), (15, "SIGTERM")];
/// `SIG_DFL`.
const SIG_DFL: usize = 0;
/// How often the watcher looks for a stop signal.
const WATCH: Duration = Duration::from_millis(100);

/// The last stop signal that arrived, 0 for none. Process-wide because a signal handler has no context; the handler only
/// stores it (async-signal-safe).
static PENDING: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_stop_signal(sig: c_int) {
    PENDING.store(sig, Ordering::Relaxed);
    // SAFETY: `signal` is async-signal-safe; the next signal of this kind takes the default action.
    unsafe { signal(sig, SIG_DFL) };
}

/// Turns the stop signals into a stop request: the handler marks them, a thread asks for the stop.
fn stop_on_signals() {
    for (sig, name) in STOP_SIGNALS {
        // SAFETY: `on_stop_signal` is an `extern "C" fn(c_int)` that only stores an atomic and calls `signal`.
        if unsafe { signal(sig, on_stop_signal as extern "C" fn(c_int) as usize) } == usize::MAX {
            eprintln!("nxkit-host: {name} ends the process without writing the pipeline cache");
        }
    }
    let watcher = std::thread::Builder::new()
        .name("nxkit-stopsig".into())
        .spawn(|| {
            loop {
                std::thread::sleep(WATCH);
                let sig = PENDING.load(Ordering::Relaxed);
                if let Some((_, name)) = STOP_SIGNALS.iter().find(|(s, _)| *s == sig) {
                    crate::stop::request(&format!("{name} arrived"));
                    return;
                }
            }
        });
    if let Err(e) = watcher {
        eprintln!("nxkit-host: the stop signal thread did not start: {e}");
    }
}

extern "C" fn at_exit() {
    // A panic must not unwind out of an `extern "C"` function.
    let _ = std::panic::catch_unwind(nxkit_gpu::flush_runtime_cache);
    let _ = std::panic::catch_unwind(crate::savesync::at_exit);
}

/// Registers the handler and the stop signals, once.
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: `at_exit` is an `extern "C"` function without arguments that lives for the whole process.
        if unsafe { atexit(at_exit) } != 0 {
            eprintln!("nxkit-host: the pipeline cache is not written at the exit (atexit failed)");
        }
        stop_on_signals();
    });
}
