//! The write of the runtime pipeline cache when the process ends.
//!
//! The host ends with `std::process::exit` (the main thread, the watchdog of `stop.rs`, a guest `ExitProcess`), which
//! runs the C library's `atexit` handlers but no destructor of the GPU backend. One handler writes what the run has
//! compiled (`nxkit_gpu::flush_runtime_cache`), so the next start finds it.

use std::sync::Once;

unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::ffi::c_int;
}

extern "C" fn at_exit() {
    // A panic must not unwind out of an `extern "C"` function.
    let _ = std::panic::catch_unwind(nxkit_gpu::flush_runtime_cache);
}

/// Registers the handler, once.
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: `at_exit` is an `extern "C"` function without arguments that lives for the whole process.
        if unsafe { atexit(at_exit) } != 0 {
            eprintln!("nxkit-host: the pipeline cache is not written at the exit (atexit failed)");
        }
    });
}
