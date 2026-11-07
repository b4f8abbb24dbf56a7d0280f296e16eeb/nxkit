//! The graceful exit of the host. Source: nxnative `dm/host/hle_lifecycle.c` (`dm_host_shutdown_then`).
//!
//! The host calls [`quit`] when the player closes the game or the process gets SIGTERM. `nn::oe::ExecuteProgram` of the
//! game ends in it too, when the host has no other program to start.

use super::log;
use crate::seams::seams;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long the graceful exit may take before the process ends at once.
const GRACE: Duration = Duration::from_millis(1500);

static QUITTING: AtomicBool = AtomicBool::new(false);

/// Ends the process after the host stopped the audio and the graphics (the `quit` hook of the seams), with the exit
/// code 0. A second call returns at once. A watchdog thread ends the process after 1.5 seconds when the hook hangs.
pub fn quit(why: &str) {
    if QUITTING.swap(true, Ordering::SeqCst) {
        return;
    }
    log(&format!("received {why}: graceful exit"));
    let watchdog = std::thread::Builder::new().name("nxkit-quit".to_owned()).spawn(|| {
        std::thread::sleep(GRACE);
        log("graceful exit not done in 1.5 seconds, exiting at once");
        std::process::exit(0)
    });
    if watchdog.is_err() {
        log("no watchdog thread for the graceful exit");
    }
    if let Some(hook) = seams().quit {
        hook();
    }
    log("graceful exit done");
    std::process::exit(0)
}
