//! Process stop. Any thread asks, the main thread ends the application, and a watchdog ends the process when that
//! does not happen.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::time::Duration;

/// How long the application gets to stop before the watchdog ends the process.
const GRACE: Duration = Duration::from_secs(5);

fn cell() -> &'static Arc<AtomicBool> {
    static CELL: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    CELL.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

/// The stop flag. The window loop reads it on every display tick.
pub fn flag() -> Arc<AtomicBool> {
    cell().clone()
}

/// True once a stop was requested.
pub fn requested() -> bool {
    cell().load(Ordering::Acquire)
}

/// Asks the host to stop. The reason goes to stderr. The first call arms the watchdog.
pub fn request(why: &str) {
    static ARM: Once = Once::new();
    ARM.call_once(|| {
        eprintln!("nxkit-host: stopping, {why}");
        cell().store(true, Ordering::Release);
        std::thread::spawn(|| {
            std::thread::sleep(GRACE);
            eprintln!(
                "nxkit-host: the window did not stop in {} s, ending the process",
                GRACE.as_secs()
            );
            std::process::exit(0);
        });
    });
}

/// Stops the calling thread for good. The process ends from the main thread.
pub fn park_forever() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Ends the process with an error message. Used where the host cannot go on and cannot return an error (R15).
pub fn fatal(message: &str) -> ! {
    eprintln!("nxkit-host: fatal: {message}");
    std::process::exit(2);
}
