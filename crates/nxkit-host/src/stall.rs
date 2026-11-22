//! The stall report (`NXKIT_STALL_DUMP=<seconds>`): when no present went on screen for that long, the host prints
//! the kernel's report of every guest thread (`Kernel::thread_report`), once per stall.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The time of the latest present that went on screen, on the clock of the NVN layer (`nxkit_rt::nvn::clock_ns`),
/// or 0 before the first. A Vulkan game's presents are on another clock, so the watch is for NVN games.
// Process-wide (W26): the shown hook of the screen is a plain function without context.
static LAST_SHOWN_NS: AtomicU64 = AtomicU64::new(0);

/// A present went on screen at `now_ns`.
pub fn shown(now_ns: u64) {
    LAST_SHOWN_NS.store(now_ns.max(1), Ordering::Relaxed);
}

/// Starts the watch thread when `secs` is set.
pub fn start(secs: Option<f64>) {
    let Some(secs) = secs.filter(|s| *s > 0.0) else {
        return;
    };
    let limit = (secs * 1e9) as u64;
    let spawned = std::thread::Builder::new()
        .name("nxkit-stall".into())
        .spawn(move || {
            let mut reported = 0;
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let last = LAST_SHOWN_NS.load(Ordering::Relaxed);
                let now = nxkit_rt::nvn::clock_ns();
                if last == 0 || last == reported || now.saturating_sub(last) < limit {
                    continue;
                }
                reported = last;
                if let Some(k) = nxkit_rt::kernel() {
                    eprintln!(
                        "nxkit-host: stall: no present for {} ms\n{}",
                        (now - last) / 1_000_000,
                        k.thread_report()
                    );
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("nxkit-host: the stall watch did not start: {e}");
    }
}
