//! The target frame rate and how the game's frames are held on screen.
//!
//! These are the pure rules of nxnative that tie the profile keys `fps_target`, `fps_max` and `present_interval` to the
//! present path. They are ported from `dm/gfx/nvn/hle_nvn.c` (`fps_high`, `fps_present_cap`, `present_itv`,
//! `rt_period_ns`) and `dm/host/hle_fpslock.c` of nxnative, read on 2026-11-07. The host reads the title's facts and calls
//! them once at the start, and `present_vsyncs` on every present.

use crate::notice::Notice;

/// The target frame rates that a profile may ask for.
pub const TARGETS: [u32; 4] = [0, 30, 60, 120];
/// The highest target that needs `fps_max` of at least 120.
pub const HIGH_TARGET: u32 = 120;

/// The target after its checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// 0 leaves the game's own limit. Otherwise 30, 60 or 120.
    pub fps: u32,
    /// What was refused.
    pub notice: Option<Notice>,
}

/// Checks the profile's `fps_target` against its `fps_max`. A target that is not 0, 30, 60 or 120 leaves the game's own
/// limit. A target of 120 needs `fps_max` of 120 or more, because a game's speed at 120 fps is checked per game, and it
/// runs at 60 without.
pub fn resolve_target(fps_target: u32, fps_max: u32) -> Target {
    if !TARGETS.contains(&fps_target) {
        return Target {
            fps: 0,
            notice: Some(Notice::red(format!(
                "the target frame rate {fps_target} must be 0, 30, 60 or 120, so the game's frame limit stays"
            ))),
        };
    }
    if fps_target == HIGH_TARGET && fps_max < HIGH_TARGET {
        return Target {
            fps: 60,
            notice: Some(Notice::red(
                "the target frame rate 120 is not checked for this game's speed (fps_max below 120), so it runs at 60",
            )),
        };
    }
    Target {
        fps: fps_target,
        notice: None,
    }
}

/// Whether the game runs at 120 fps through an engine limiter (`fps_high`). It needs a target of 120, an `fps_max` of 120
/// or more, no frame step state page (the stubs of a state page hand the game whole vsyncs of 1/60 s) and an engine
/// limiter that the host writes. The notice says why a target of 120 does not apply.
pub fn runs_at_120(
    fps_target: u32,
    fps_max: u32,
    has_state_page: bool,
    has_engine_limiter: bool,
) -> (bool, Option<Notice>) {
    if fps_target != HIGH_TARGET || fps_max < HIGH_TARGET {
        return (false, None);
    }
    let high = !has_state_page && has_engine_limiter;
    let why = (!high).then(|| {
        Notice::red(format!(
            "120 fps: this main has {}, so it runs at 60",
            if has_state_page {
                "a frame step in whole vsyncs"
            } else {
                "no engine frame limit the host writes"
            }
        ))
    });
    (high, why)
}

/// The present cap of a game that has an engine limiter and no frame step (`fps_present_cap`). 120 is not this game's
/// (it does not run at 120 fps), so it runs at 60. Without an engine limiter nothing makes the game follow a cap, so the
/// game's own present interval stays and the notice says so.
pub fn engine_present_cap(fps_target: u32, has_engine_limiter: bool) -> (u32, Option<Notice>) {
    let cap = match fps_target {
        30 | 60 => fps_target,
        120 => 60,
        _ => 0,
    };
    if cap != 0 && !has_engine_limiter {
        let n = Notice::red(format!(
            "present cap {cap} fps not applied: this main has neither a frame step nor an engine frame limit the host writes, so the game's own present interval stays"
        ));
        return (0, Some(n));
    }
    (cap, None)
}

/// What decides how long a frame stays on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentRule {
    /// The present cap in fps, 0 for none (`fps_present_cap`).
    pub cap: u32,
    /// The hold that the frame step decided, in units of `step_hz`. 0 when it has not decided yet.
    pub hold: u32,
    /// Whether the host decides the steps in whole vsyncs (the frame step runs).
    pub step_on: bool,
    /// The units of the frame step: 60, or 120 when the game runs at 120 fps.
    pub step_hz: u32,
    /// `present_interval` of the profile is `display`: one frame per tick of the display link.
    pub display_mode: bool,
}

impl PresentRule {
    /// The vsyncs that a frame stays on screen, in units of `1 / step_hz` s (`present_itv`). `game_interval` is the
    /// present interval that the game set with `nvnWindowSetPresentInterval`, 0 when it set none.
    ///
    /// With a cap the frame step's hold wins and the game's interval does not count. Without a cap `present_interval`
    /// of the profile says whether a frame follows the game's interval (`game`, as on the Switch) or the display
    /// (`display`).
    pub fn vsyncs(&self, game_interval: u32) -> u32 {
        if let Some(per_cap) = self.step_hz.checked_div(self.cap) {
            if self.hold != 0 && self.step_on {
                self.hold
            } else {
                per_cap
            }
        } else if self.display_mode {
            1
        } else {
            game_interval.max(1)
        }
    }

    /// The hold that the thread of the acquire declares to the scheduler as its deadline, in the units of the rule.
    ///
    /// A hold that the frame step raised for late frames keeps the deadline of the cap. A deadline of the longer hold let
    /// the performance controller of a phone move the game to the efficiency cores at half the clock, where 60 fps no
    /// longer fitted and every trial back to it failed. A step that the game fixes itself is declared as it is.
    pub fn deadline_vsyncs(&self, game_interval: u32, game_fixes_step: bool) -> u32 {
        let itv = self.vsyncs(game_interval);
        if self.cap == 0 || !self.step_on || game_fixes_step {
            return itv;
        }
        let kmin = self.step_hz / self.cap;
        if itv > kmin { kmin } else { itv }
    }
}

/// The target that the engine limiters take at 120 fps: 120 while the hold is one tick, else 60 (`nvn_fps_high_now`). The
/// game's step stays what the screen shows.
pub fn high_target_now(hold: u32) -> u32 {
    if hold == 1 { 120 } else { 60 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_checked_against_fps_max() {
        assert_eq!(resolve_target(60, 60).fps, 60);
        assert_eq!(resolve_target(0, 60).fps, 0);
        assert!(resolve_target(120, 60).notice.is_some());
        assert_eq!(resolve_target(120, 60).fps, 60);
        assert_eq!(resolve_target(120, 120).fps, 120);
        let bad = resolve_target(45, 120);
        assert!(bad.fps == 0 && bad.notice.is_some_and(|n| n.red));
    }

    #[test]
    fn a_state_page_or_a_missing_limiter_keeps_120_out() {
        assert_eq!(runs_at_120(120, 120, false, true), (true, None));
        let (high, why) = runs_at_120(120, 120, true, true);
        assert!(!high && why.is_some_and(|n| n.text.contains("whole vsyncs")));
        let (high, why) = runs_at_120(120, 120, false, false);
        assert!(!high && why.is_some_and(|n| n.text.contains("no engine frame limit")));
        assert_eq!(runs_at_120(60, 120, false, true), (false, None));
        assert_eq!(runs_at_120(120, 60, false, true), (false, None));
    }

    #[test]
    fn an_engine_cap_needs_a_limiter() {
        assert_eq!(engine_present_cap(30, true), (30, None));
        assert_eq!(engine_present_cap(120, true), (60, None));
        assert_eq!(engine_present_cap(0, false), (0, None));
        let (cap, why) = engine_present_cap(60, false);
        assert!(cap == 0 && why.is_some_and(|n| n.red));
    }

    fn rule(cap: u32, hold: u32, step_on: bool, display_mode: bool) -> PresentRule {
        PresentRule {
            cap,
            hold,
            step_on,
            step_hz: 60,
            display_mode,
        }
    }

    #[test]
    fn the_hold_wins_over_the_game_interval_under_a_cap() {
        assert_eq!(rule(60, 2, true, false).vsyncs(1), 2);
        assert_eq!(
            rule(60, 0, true, false).vsyncs(2),
            1,
            "not decided yet: the cap's own"
        );
        assert_eq!(rule(30, 0, true, false).vsyncs(1), 2);
        assert_eq!(
            rule(30, 3, false, false).vsyncs(1),
            2,
            "an engine cap has no hold"
        );
    }

    #[test]
    fn without_a_cap_the_profile_says_who_decides() {
        assert_eq!(rule(0, 0, false, false).vsyncs(2), 2, "game");
        assert_eq!(rule(0, 0, false, false).vsyncs(0), 1);
        assert_eq!(rule(0, 0, false, true).vsyncs(2), 1, "display");
    }

    #[test]
    fn a_raised_hold_keeps_the_deadline_of_the_cap() {
        assert_eq!(rule(60, 2, true, false).deadline_vsyncs(1, false), 1);
        assert_eq!(rule(60, 2, true, false).deadline_vsyncs(1, true), 2);
        assert_eq!(rule(30, 2, true, false).deadline_vsyncs(1, false), 2);
        assert_eq!(rule(0, 0, false, false).deadline_vsyncs(2, false), 2);
    }

    #[test]
    fn the_engine_target_follows_the_hold() {
        assert_eq!(high_target_now(1), 120);
        assert_eq!(high_target_now(2), 60);
        assert_eq!(high_target_now(0), 60);
    }
}
