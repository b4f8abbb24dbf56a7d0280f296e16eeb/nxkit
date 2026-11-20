//! The diagnostics of the present path of one backend, with the switches of `NXKIT_DIAG` read once when it is made.
//!
//! - `present`: the counters of the ticks, the presents asked, the textures found and the blits done.
//! - `encpace`: for every 60th frame, the time from the game's present to the end of its encode and from there to the
//!   tick that shows it, to see where a deeper pipeline loses time.
//! - `overlap` and `stats`: whether the backend prints those counters.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const N: usize = 256;

/// The state of the present diagnostics. The backend owns one.
pub(super) struct PresentDiag {
    present: bool,
    encpace: bool,
    /// `NXKIT_DIAG=overlap`.
    pub(super) overlap: bool,
    /// `NXKIT_DIAG=stats`.
    pub(super) stats: bool,
    /// Ticks, presents asked, textures found, blits done.
    counts: [AtomicU64; 4],
    base: Instant,
    asked: AtomicU64,
    at_ask: [AtomicU64; N],
    at_ready: [AtomicU64; N],
}

impl Default for PresentDiag {
    fn default() -> Self {
        PresentDiag {
            present: crate::diag("present"),
            encpace: crate::diag("encpace"),
            overlap: crate::diag("overlap"),
            stats: crate::diag("stats"),
            counts: [const { AtomicU64::new(0) }; 4],
            base: Instant::now(),
            asked: AtomicU64::new(0),
            at_ask: [const { AtomicU64::new(0) }; N],
            at_ready: [const { AtomicU64::new(0) }; N],
        }
    }
}

impl PresentDiag {
    fn now(&self) -> u64 {
        u64::try_from(self.base.elapsed().as_micros()).unwrap_or(0)
    }

    /// Counts one event of the present path: 0 a tick, 1 a present asked, 2 a texture found, 3 a blit done. Every 120th
    /// tick prints the counts.
    pub(super) fn count(&self, i: usize) {
        if !self.present {
            return;
        }
        let n = self.counts[i].fetch_add(1, Ordering::Relaxed) + 1;
        if i == 0 && n.is_multiple_of(120) {
            let v: Vec<u64> = self
                .counts
                .iter()
                .map(|c| c.load(Ordering::Relaxed))
                .collect();
            eprintln!(
                "DIAG present ticks {} asked {} found {} blitted {}",
                v[0], v[1], v[2], v[3]
            );
        }
    }

    /// The number of textures found for a tick so far.
    pub(super) fn found(&self) -> u64 {
        self.counts[2].load(Ordering::Relaxed)
    }

    /// The game presented its next frame.
    pub(super) fn asked(&self) {
        if self.encpace {
            let n = self.asked.fetch_add(1, Ordering::Relaxed) + 1;
            self.at_ask[n as usize % N].store(self.now(), Ordering::Relaxed);
        }
    }

    /// Frame `n` is encoded and queued for the screen.
    pub(super) fn ready(&self, n: u64) {
        if self.encpace {
            self.at_ready[n as usize % N].store(self.now(), Ordering::Relaxed);
        }
    }

    /// A tick shows frame `n`, with `pending` frames queued.
    pub(super) fn shown(&self, n: u64, pending: usize) {
        if self.encpace && n.is_multiple_of(60) {
            let t = self.now();
            let ask = self.at_ask[n as usize % N].load(Ordering::Relaxed);
            let ready = self.at_ready[n as usize % N].load(Ordering::Relaxed);
            eprintln!(
                "DIAG encpace frame {n}: present->ready {:.2} ms, ready->shown {:.2} ms, asked {} pending {pending}",
                ready.saturating_sub(ask) as f64 / 1e3,
                t.saturating_sub(ready) as f64 / 1e3,
                self.asked.load(Ordering::Relaxed),
            );
        }
    }
}
