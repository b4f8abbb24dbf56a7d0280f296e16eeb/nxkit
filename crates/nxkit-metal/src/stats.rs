//! Pacing measurement (R16): the cost of a tick, missed ticks and the interval between shown frames.
//!
//! The counters are atomics, so the tick handler on the main thread and the presented handler on a Metal thread write
//! them without a lock. The cost is a few atomic additions per tick, so the counters stay on.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::schedule::{Shown, TickAction};

/// The number of buckets of the interval histogram.
pub const INTERVAL_BUCKETS: usize = 8;

/// The upper bounds of the first seven buckets in microseconds. The eighth bucket holds everything above.
///
/// The bounds put 120 Hz (8.3 ms), 60 Hz (16.7 ms) and 30 Hz (33.3 ms) in buckets of their own.
pub const INTERVAL_BOUNDS_US: [u64; INTERVAL_BUCKETS - 1] =
    [6_000, 10_000, 14_000, 20_000, 25_000, 36_000, 50_000];

/// The labels of the buckets.
pub const INTERVAL_LABELS: [&str; INTERVAL_BUCKETS] = [
    "<6 ms", "6-10 ms", "10-14 ms", "14-20 ms", "20-25 ms", "25-36 ms", "36-50 ms", ">=50 ms",
];

/// The counters.
#[derive(Debug, Default)]
pub struct PacingStats {
    ticks: AtomicU64,
    game_frames: AtomicU64,
    interpolated_frames: AtomicU64,
    held: AtomicU64,
    missed: AtomicU64,
    handler_total_ns: AtomicU64,
    handler_max_ns: AtomicU64,
    shown: AtomicU64,
    skipped_by_display: AtomicU64,
    late_total_ns: AtomicU64,
    late_max_ns: AtomicU64,
    last_shown_bits: AtomicU64,
    last_interval_ns: AtomicU64,
    histogram: [AtomicU64; INTERVAL_BUCKETS],
}

/// A copy of the counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PacingSnapshot {
    /// The ticks of the display link.
    pub ticks: u64,
    /// The ticks that showed a game frame.
    pub game_frames: u64,
    /// The ticks that showed an interpolated frame.
    pub interpolated_frames: u64,
    /// The ticks where nothing was due.
    pub held: u64,
    /// The ticks where a frame was due and none was ready.
    pub missed: u64,
    /// The sum of the time the host spent in the tick handler, in nanoseconds.
    pub handler_total_ns: u64,
    /// The longest time in the tick handler.
    pub handler_max_ns: u64,
    /// The frames the display reported as shown.
    pub shown: u64,
    /// The frames that were presented and that the display skipped.
    pub skipped_by_display: u64,
    /// The sum of how late the shown frames were against their target time, in nanoseconds.
    pub late_total_ns: u64,
    /// The largest lateness of a shown frame.
    pub late_max_ns: u64,
    /// The interval between the last two shown frames.
    pub last_interval_ns: u64,
    /// The count of shown-frame intervals per bucket of [`INTERVAL_BOUNDS_US`].
    pub histogram: [u64; INTERVAL_BUCKETS],
}

impl PacingSnapshot {
    /// The mean time in the tick handler in nanoseconds, or 0 without ticks.
    pub fn mean_handler_ns(&self) -> u64 {
        self.handler_total_ns.checked_div(self.ticks).unwrap_or(0)
    }

    /// The mean lateness of the shown frames in nanoseconds, or 0 without shown frames.
    pub fn mean_late_ns(&self) -> u64 {
        self.late_total_ns.checked_div(self.shown).unwrap_or(0)
    }
}

/// The bucket of an interval in microseconds.
pub fn interval_bucket(us: u64) -> usize {
    INTERVAL_BOUNDS_US
        .iter()
        .position(|b| us < *b)
        .unwrap_or(INTERVAL_BUCKETS - 1)
}

fn secs_to_ns(s: f64) -> u64 {
    if s.is_finite() && s > 0.0 {
        (s * 1e9) as u64
    } else {
        0
    }
}

impl PacingStats {
    /// Counts one tick and its action.
    pub fn record_tick(&self, action: &TickAction) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        let counter = match action {
            TickAction::Present(Shown::Game { .. }) => &self.game_frames,
            TickAction::Present(Shown::Interpolated { .. }) => &self.interpolated_frames,
            TickAction::Hold => &self.held,
            TickAction::Missed => &self.missed,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds the time the host spent in the tick handler.
    pub fn record_handler(&self, ns: u64) {
        self.handler_total_ns.fetch_add(ns, Ordering::Relaxed);
        self.handler_max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Records a frame that the display reported.
    ///
    /// `shown` is `presentedTime` of the drawable in seconds on the host clock. It is 0 when the display skipped the
    /// frame. `target` is the time the frame was meant for, when the present gave one.
    pub fn record_presented(&self, target: Option<f64>, shown: f64) {
        if !shown.is_finite() || shown <= 0.0 {
            self.skipped_by_display.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.shown.fetch_add(1, Ordering::Relaxed);
        if let Some(t) = target {
            let late = secs_to_ns(shown - t);
            self.late_total_ns.fetch_add(late, Ordering::Relaxed);
            self.late_max_ns.fetch_max(late, Ordering::Relaxed);
        }
        let prev = self
            .last_shown_bits
            .swap(shown.to_bits(), Ordering::Relaxed);
        if prev != 0 {
            let ns = secs_to_ns(shown - f64::from_bits(prev));
            self.last_interval_ns.store(ns, Ordering::Relaxed);
            self.histogram[interval_bucket(ns / 1_000)].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Reads the counters. The values are not one atomic snapshot, which is fine for a display.
    pub fn snapshot(&self) -> PacingSnapshot {
        let mut histogram = [0; INTERVAL_BUCKETS];
        for (out, c) in histogram.iter_mut().zip(&self.histogram) {
            *out = c.load(Ordering::Relaxed);
        }
        PacingSnapshot {
            ticks: self.ticks.load(Ordering::Relaxed),
            game_frames: self.game_frames.load(Ordering::Relaxed),
            interpolated_frames: self.interpolated_frames.load(Ordering::Relaxed),
            held: self.held.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            handler_total_ns: self.handler_total_ns.load(Ordering::Relaxed),
            handler_max_ns: self.handler_max_ns.load(Ordering::Relaxed),
            shown: self.shown.load(Ordering::Relaxed),
            skipped_by_display: self.skipped_by_display.load(Ordering::Relaxed),
            late_total_ns: self.late_total_ns.load(Ordering::Relaxed),
            late_max_ns: self.late_max_ns.load(Ordering::Relaxed),
            last_interval_ns: self.last_interval_ns.load(Ordering::Relaxed),
            histogram,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_split_at_the_bounds() {
        assert_eq!(interval_bucket(5_999), 0);
        assert_eq!(interval_bucket(8_333), 1);
        assert_eq!(interval_bucket(16_667), 3);
        assert_eq!(interval_bucket(33_333), 5);
        assert_eq!(interval_bucket(50_000), 7);
        assert_eq!(interval_bucket(u64::MAX), 7);
    }

    #[test]
    fn ticks_split_by_action() {
        let s = PacingStats::default();
        s.record_tick(&TickAction::Present(Shown::Game { frame: 1 }));
        s.record_tick(&TickAction::Present(Shown::Interpolated { from: 1, to: 2 }));
        s.record_tick(&TickAction::Hold);
        s.record_tick(&TickAction::Missed);
        let n = s.snapshot();
        assert_eq!(
            (
                n.ticks,
                n.game_frames,
                n.interpolated_frames,
                n.held,
                n.missed
            ),
            (4, 1, 1, 1, 1)
        );
    }

    #[test]
    fn shown_frames_fill_the_histogram() {
        let s = PacingStats::default();
        s.record_presented(Some(10.0), 10.001);
        s.record_presented(Some(10.008), 10.0093);
        s.record_presented(None, 0.0);
        let n = s.snapshot();
        assert_eq!((n.shown, n.skipped_by_display), (2, 1));
        assert_eq!(n.histogram[1], 1);
        assert_eq!(n.histogram.iter().sum::<u64>(), 1);
        assert!(n.late_max_ns >= 1_000_000);
    }

    #[test]
    fn means_do_not_divide_by_zero() {
        let n = PacingSnapshot::default();
        assert_eq!((n.mean_handler_ns(), n.mean_late_ns()), (0, 0));
    }
}
