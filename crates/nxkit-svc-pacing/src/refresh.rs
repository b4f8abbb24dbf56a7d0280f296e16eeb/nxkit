//! How many display ticks one game frame takes, for any refresh rate of the screen.
//!
//! The frame step counts in vsyncs of 1/60 s. The display link ticks at a rate that the host picks from the refresh
//! rate of the screen. [`plan_ticks`] turns the refresh rate and the frame rate of the game into that rate and a whole
//! number of ticks per frame, so the scheduler can hold each frame exactly. A [`RateTracker`] measures the real tick
//! rate, because the rate that the system reports and the rate that the link runs at can differ.
//!
//! Nothing here touches the system. The host feeds it numbers.

use std::collections::VecDeque;

/// How far a refresh rate may be from a whole multiple of the frame rate and still count as one. 59.94 Hz and 119.88
/// Hz (NTSC timings) are 0.1% off.
const EVEN_TOLERANCE: f64 = 0.005;

/// What the display link does for a game frame rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TickPlan {
    /// The rate to ask the display link for, in Hz.
    pub link_hz: u32,
    /// The ticks that one game frame stays on screen.
    pub ticks_per_frame: u32,
    /// The game frame rate this plan gives, in frames per second.
    pub fps: u32,
    /// Whether each frame stays exactly the same time. When it is false, the screen alternates between two cadences
    /// (judder) and `switch_to` names a refresh rate that would fix it.
    pub exact: bool,
    /// A refresh rate of the display that is an even multiple of the target, when the current one is not. It is the
    /// multiple nearest to the current rate. The host looks for the mode of the display that comes nearest to it.
    pub switch_to: Option<u32>,
}

/// The whole number `n` that `refresh / target` is within [`EVEN_TOLERANCE`] of, when there is one.
fn even_multiple(refresh: f64, target: u32) -> Option<u32> {
    if !(refresh.is_finite() && refresh > 0.0) || target == 0 {
        return None;
    }
    let ratio = refresh / f64::from(target);
    let n = ratio.round();
    (n >= 1.0 && (ratio - n).abs() <= EVEN_TOLERANCE * n).then_some(n as u32)
}

/// The plan for a screen that shows `refresh_hz` and a game that aims at `target_fps`.
///
/// - `adaptive`: the screen changes its refresh rate with the link (ProMotion, adaptive sync). The link asks for the
///   target itself, one tick per frame, and the screen follows. The host adds a timed present.
/// - Otherwise a refresh rate that is an even multiple of the target gives `multiple` ticks per frame.
/// - Otherwise, with `free_rate` (the game accepts any frame rate), the nearest even split wins: 144 Hz gives 72 fps
///   (2 ticks), not 48 (3 ticks), because the first is nearer to 60 and a tie goes to the faster one.
/// - Otherwise the game has a fixed rate that the screen cannot show evenly. The plan asks for the target, says it is
///   not exact, and names a refresh rate to switch to.
pub fn plan_ticks(refresh_hz: f64, target_fps: u32, free_rate: bool, adaptive: bool) -> TickPlan {
    let target = target_fps.max(1);
    if adaptive {
        return TickPlan {
            link_hz: target,
            ticks_per_frame: 1,
            fps: target,
            exact: true,
            switch_to: None,
        };
    }
    let refresh = if refresh_hz.is_finite() && refresh_hz > 0.0 {
        refresh_hz
    } else {
        f64::from(target)
    };
    let link = refresh.round().max(1.0) as u32;
    if let Some(n) = even_multiple(refresh, target) {
        return TickPlan {
            link_hz: link,
            ticks_per_frame: n,
            fps: target,
            exact: true,
            switch_to: None,
        };
    }
    let ratio = refresh / f64::from(target);
    let below = (ratio.floor() as u32).max(1);
    if free_rate {
        // The candidates are the even splits just below and above the ratio; the nearer rate to the target wins.
        let rate = |m: u32| refresh / f64::from(m);
        let miss = |m: u32| (rate(m) - f64::from(target)).abs();
        let m = if miss(below) <= miss(below + 1) {
            below
        } else {
            below + 1
        };
        return TickPlan {
            link_hz: link,
            ticks_per_frame: m,
            fps: rate(m).round() as u32,
            exact: true,
            switch_to: None,
        };
    }
    let n = (ratio.round() as u32).max(1);
    TickPlan {
        link_hz: target,
        ticks_per_frame: 1,
        fps: target,
        exact: false,
        switch_to: Some(target * n),
    }
}

/// Measures the tick rate of the display link from the target times of its ticks.
///
/// It keeps the last `WINDOW` gaps. A gap more than 1.5 times the median (a stall, a missed tick) is left out, so one
/// hiccup does not change the rate.
#[derive(Debug, Default)]
pub struct RateTracker {
    last: Option<f64>,
    gaps: VecDeque<f64>,
}

impl RateTracker {
    /// The gaps that the tracker keeps.
    pub const WINDOW: usize = 60;
    /// The gaps it needs before it gives a rate.
    pub const NEEDED: usize = 30;

    /// An empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// A tick with the target time `secs` (seconds on any clock that only goes forward).
    pub fn tick(&mut self, secs: f64) {
        if let Some(prev) = self.last
            && secs > prev
        {
            if self.gaps.len() == Self::WINDOW {
                self.gaps.pop_front();
            }
            self.gaps.push_back(secs - prev);
        }
        self.last = Some(secs);
    }

    /// Forgets the gaps, for a change of the screen or its mode.
    pub fn reset(&mut self) {
        self.last = None;
        self.gaps.clear();
    }

    /// The measured tick rate in Hz, once enough ticks came.
    pub fn hz(&self) -> Option<f64> {
        if self.gaps.len() < Self::NEEDED {
            return None;
        }
        let mut sorted: Vec<f64> = self.gaps.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2];
        let kept: Vec<f64> = sorted.into_iter().filter(|g| *g <= median * 1.5).collect();
        let mean = kept.iter().sum::<f64>() / kept.len() as f64;
        (mean > 0.0).then(|| 1.0 / mean)
    }
}

/// Whether a measured rate differs from the expected one by more than 4%, so the host must plan again.
pub fn rate_differs(measured_hz: f64, expected_hz: f64) -> bool {
    expected_hz > 0.0 && ((measured_hz - expected_hz) / expected_hz).abs() > 0.04
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_60_fps_game_takes_two_ticks_at_120_hz() {
        let p = plan_ticks(120.0, 60, false, false);
        assert_eq!(
            (p.link_hz, p.ticks_per_frame, p.fps, p.exact, p.switch_to),
            (120, 2, 60, true, None)
        );
    }

    #[test]
    fn ntsc_timings_count_as_even() {
        assert_eq!(plan_ticks(119.88, 60, false, false).ticks_per_frame, 2);
        assert_eq!(plan_ticks(59.94, 60, false, false).ticks_per_frame, 1);
    }

    #[test]
    fn a_fixed_rate_game_on_144_hz_is_not_exact_and_names_120() {
        let p = plan_ticks(144.0, 60, false, false);
        assert!(!p.exact);
        assert_eq!(
            (p.link_hz, p.ticks_per_frame, p.switch_to),
            (60, 1, Some(120))
        );
    }

    #[test]
    fn a_free_rate_game_on_144_hz_runs_at_72() {
        let p = plan_ticks(144.0, 60, true, false);
        assert_eq!(
            (p.link_hz, p.ticks_per_frame, p.fps, p.exact),
            (144, 2, 72, true)
        );
    }

    #[test]
    fn a_free_rate_game_on_100_hz_picks_the_nearest_split() {
        // 100 / 2 = 50 is 10 off 60, 100 / 1 = 100 is 40 off.
        let p = plan_ticks(100.0, 60, true, false);
        assert_eq!((p.ticks_per_frame, p.fps), (2, 50));
    }

    #[test]
    fn a_30_fps_game_on_144_hz_gets_a_split_that_fits() {
        let p = plan_ticks(144.0, 30, false, false);
        assert!(!p.exact);
        assert_eq!(p.switch_to, Some(150));
        let p = plan_ticks(120.0, 30, false, false);
        assert_eq!((p.ticks_per_frame, p.exact), (4, true));
    }

    #[test]
    fn an_adaptive_screen_follows_the_target() {
        let p = plan_ticks(120.0, 60, false, true);
        assert_eq!((p.link_hz, p.ticks_per_frame, p.exact), (60, 1, true));
    }

    #[test]
    fn a_bad_refresh_falls_back_to_the_target() {
        let p = plan_ticks(0.0, 60, false, false);
        assert_eq!((p.link_hz, p.ticks_per_frame), (60, 1));
        let p = plan_ticks(f64::NAN, 60, false, false);
        assert_eq!(p.ticks_per_frame, 1);
    }

    #[test]
    fn the_tracker_measures_120_hz_and_ignores_a_stall() {
        let mut t = RateTracker::new();
        assert_eq!(t.hz(), None);
        let mut now = 100.0;
        for i in 0..80 {
            now += if i == 40 { 0.05 } else { 1.0 / 120.0 };
            t.tick(now);
        }
        let hz = t.hz().unwrap_or(0.0);
        assert!((hz - 120.0).abs() < 0.5, "{hz}");
        t.reset();
        assert_eq!(t.hz(), None);
    }

    #[test]
    fn a_rate_that_is_off_by_more_than_4_percent_differs() {
        assert!(!rate_differs(119.5, 120.0));
        assert!(rate_differs(60.0, 120.0));
        assert!(rate_differs(144.0, 120.0));
    }
}
