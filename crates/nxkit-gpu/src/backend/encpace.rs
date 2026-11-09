//! `NXKIT_DIAG=encpace`: for every 60th frame, the time from the game's present to the end of its encode and from there
//! to the tick that shows it, to see where a deeper pipeline loses time.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const N: usize = 256;
static ASKED: AtomicU64 = AtomicU64::new(0);
static AT_ASK: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static AT_READY: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::diag("encpace"))
}

fn now() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    u64::try_from(BASE.get_or_init(Instant::now).elapsed().as_micros()).unwrap_or(0)
}

/// The game presented its next frame.
pub(super) fn asked() {
    if on() {
        let n = ASKED.fetch_add(1, Ordering::Relaxed) + 1;
        AT_ASK[n as usize % N].store(now(), Ordering::Relaxed);
    }
}

/// Frame `n` is encoded and queued for the screen.
pub(super) fn ready(n: u64) {
    if on() {
        AT_READY[n as usize % N].store(now(), Ordering::Relaxed);
    }
}

/// A tick shows frame `n`, with `pending` frames queued.
pub(super) fn shown(n: u64, pending: usize) {
    if on() && n.is_multiple_of(60) {
        let t = now();
        let ask = AT_ASK[n as usize % N].load(Ordering::Relaxed);
        let ready = AT_READY[n as usize % N].load(Ordering::Relaxed);
        eprintln!(
            "DIAG encpace frame {n}: present->ready {:.2} ms, ready->shown {:.2} ms, asked {} pending {pending}",
            ready.saturating_sub(ask) as f64 / 1e3,
            t.saturating_sub(ready) as f64 / 1e3,
            ASKED.load(Ordering::Relaxed),
        );
    }
}
