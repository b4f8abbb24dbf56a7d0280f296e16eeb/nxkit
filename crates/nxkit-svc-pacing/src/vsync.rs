//! The display clock and the vsync event of the game.
//!
//! The game waits for the vsync event of `nn::vi::GetDisplayVsyncEvent` and steps its logic once per event. A host that
//! signals the event on its own timer drifts against the display. Two events can come in one tick of the display and
//! none in the next, and the game stutters. So the event follows the ticks of the display link (R19).
//!
//! The host calls `TickClock::tick` on every tick of the display link. A worker thread per event counts the ticks and
//! signals the event on every `ticks_per_vsync`th one. At a game rate of 60 on a display of 120 Hz that is every second
//! tick, and the game steps at the speed of the Switch. When no tick arrives for a few periods (the window is hidden and
//! the link is paused), the worker keeps the game running at the period of the game's vsync and counts that in
//! `self_clocked`, so the stall shows in the measurement. The behaviour is the one of the vsync thread of nxnative
//! (`dm/host/hle_sdk16.c`, `vsync_thread`), which also falls back to a timer.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

/// The ticks that the display link has made.
#[derive(Default)]
pub(crate) struct TickClock {
    ticks: Mutex<u64>,
    changed: Condvar,
}

impl TickClock {
    /// One tick of the display link. Returns the count of ticks.
    pub(crate) fn tick(&self) -> u64 {
        let mut t = self.ticks.lock().unwrap_or_else(PoisonError::into_inner);
        *t += 1;
        let n = *t;
        self.changed.notify_all();
        n
    }

    /// The count of ticks.
    pub(crate) fn ticks(&self) -> u64 {
        *self.ticks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until the count reaches `target` or `timeout` passes. Returns the count, or `None` on timeout.
    pub(crate) fn wait_for(&self, target: u64, timeout: Duration) -> Option<u64> {
        let t = self.ticks.lock().unwrap_or_else(PoisonError::into_inner);
        let (t, res) = self
            .changed
            .wait_timeout_while(t, timeout, |t| *t < target)
            .unwrap_or_else(PoisonError::into_inner);
        if res.timed_out() && *t < target {
            None
        } else {
            Some(*t)
        }
    }
}

/// The ticks of the display link that make one vsync of the game: the display rate over the game's vsync rate, rounded,
/// and at least 1.
pub fn ticks_per_vsync(display_hz: f32, vsync_hz: f32) -> u32 {
    if !(display_hz.is_finite() && vsync_hz.is_finite()) || display_hz <= 0.0 || vsync_hz <= 0.0 {
        return 1;
    }
    ((display_hz / vsync_hz).round() as u32).max(1)
}

/// The rates that the vsync workers read. They change when the host picks a new plan.
pub(crate) struct Rates {
    ticks_per_vsync: AtomicU32,
    vsync_period_ns: AtomicU64,
}

impl Rates {
    pub(crate) fn new(display_hz: f32, vsync_hz: f32) -> Self {
        let r = Self {
            ticks_per_vsync: AtomicU32::new(1),
            vsync_period_ns: AtomicU64::new(16_666_667),
        };
        r.set(display_hz, vsync_hz);
        r
    }

    pub(crate) fn set(&self, display_hz: f32, vsync_hz: f32) {
        self.ticks_per_vsync
            .store(ticks_per_vsync(display_hz, vsync_hz), Ordering::Relaxed);
        if vsync_hz.is_finite() && vsync_hz > 0.0 {
            self.vsync_period_ns
                .store((1e9 / f64::from(vsync_hz)) as u64, Ordering::Relaxed);
        }
    }

    pub(crate) fn ticks_per_vsync(&self) -> u32 {
        self.ticks_per_vsync.load(Ordering::Relaxed)
    }

    pub(crate) fn vsync_period(&self) -> Duration {
        Duration::from_nanos(self.vsync_period_ns.load(Ordering::Relaxed))
    }
}

/// What the waiting for one vsync did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    /// The ticks of the display link brought the vsync.
    Ticked,
    /// No tick came for four vsync periods, so the vsync comes from the timer.
    SelfClocked,
}

/// Waits for the next vsync of the game. `seen` is the count of ticks at the last vsync.
pub(crate) fn wait_vsync(clock: &TickClock, rates: &Rates, seen: &mut u64) -> Wait {
    let n = u64::from(rates.ticks_per_vsync());
    let limit = rates.vsync_period() * 4;
    match clock.wait_for(*seen + n, limit) {
        // Behind by more than one vsync: do not catch up with a burst of events.
        Some(t) => {
            *seen = t;
            Wait::Ticked
        }
        None => {
            *seen = clock.ticks();
            Wait::SelfClocked
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn a_display_of_120_hz_gives_a_game_vsync_of_60_every_second_tick() {
        assert_eq!(ticks_per_vsync(120.0, 60.0), 2);
        assert_eq!(ticks_per_vsync(60.0, 60.0), 1);
        assert_eq!(ticks_per_vsync(120.0, 120.0), 1);
        assert_eq!(ticks_per_vsync(144.0, 60.0), 2);
        assert_eq!(ticks_per_vsync(30.0, 60.0), 1);
        assert_eq!(ticks_per_vsync(0.0, 60.0), 1);
        assert_eq!(ticks_per_vsync(120.0, f32::NAN), 1);
    }

    #[test]
    fn the_wait_ends_on_the_nth_tick() {
        let clock = Arc::new(TickClock::default());
        let rates = Rates::new(120.0, 60.0);
        let c = clock.clone();
        let t = thread::spawn(move || {
            for _ in 0..4 {
                thread::sleep(Duration::from_millis(5));
                c.tick();
            }
        });
        let mut seen = 0;
        assert_eq!(wait_vsync(&clock, &rates, &mut seen), Wait::Ticked);
        assert_eq!(seen, 2);
        assert_eq!(wait_vsync(&clock, &rates, &mut seen), Wait::Ticked);
        assert_eq!(seen, 4);
        assert!(t.join().is_ok());
    }

    #[test]
    fn without_ticks_the_timer_runs_the_game_and_says_so() {
        let clock = TickClock::default();
        let rates = Rates::new(120.0, 1000.0);
        let mut seen = 0;
        assert_eq!(wait_vsync(&clock, &rates, &mut seen), Wait::SelfClocked);
    }

    #[test]
    fn a_late_wait_does_not_catch_up() {
        let clock = TickClock::default();
        let rates = Rates::new(120.0, 60.0);
        for _ in 0..9 {
            clock.tick();
        }
        let mut seen = 0;
        assert_eq!(wait_vsync(&clock, &rates, &mut seen), Wait::Ticked);
        assert_eq!(seen, 9);
    }
}
