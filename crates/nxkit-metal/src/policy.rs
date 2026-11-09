//! The pacing policy (R29).
//!
//! The policy turns the facts of a title into a [`PresentPlan`]. It says where the frames of the display come from.
//! The display runs at 120 Hz. A game that steps at the display rate needs nothing. A game at 60 Hz gets MetalFX
//! frame interpolation by default. Frames are never blended and never made by repeating without saying so.
//!
//! Every plan has a reason that the host logs, so no choice is silent (R15).

use crate::pacing::FrameInterval;

/// Who paces the game. It mirrors `nxtitle::Pacing`, which this crate does not depend on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameRate {
    /// The game's own loop.
    Game,
    /// The game steps at the display rate.
    Display,
    /// The game steps at a fixed rate.
    Fixed {
        /// Frames per second.
        hz: u32,
    },
}

/// What the policy decides on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolicyInput {
    /// The title's `pacing()` fact, or `None` when the title states none.
    pub title: Option<GameRate>,
    /// True when the title carries frame step rule data, so the host can set the game's rate.
    pub frame_step: bool,
    /// The rate of the game's own loop in frames per second. It is 60 for a Switch game unless a title fact says more.
    pub game_hz: u32,
    /// The display rate.
    pub display: FrameInterval,
    /// True when MetalFX frame interpolation can run on this device and the host has its inputs.
    pub interpolation: bool,
}

/// Where the frames of the display come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameSource {
    /// The game makes one frame for each tick.
    PerTick,
    /// The game makes a frame about every `ticks_per_frame` ticks. The display shows each frame that long.
    Hold {
        /// The least number of ticks between two game frames.
        ticks_per_frame: u32,
    },
    /// The game makes a frame every second tick. MetalFX makes the frame in between.
    Interpolate,
}

/// The plan for the present path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentPlan {
    /// The display rate.
    pub display: FrameInterval,
    /// The rate the game steps at, in frames per second.
    pub game_hz: u32,
    /// Where the frames come from.
    pub source: FrameSource,
    /// Why this plan.
    pub reason: &'static str,
}

/// Picks the plan.
pub fn choose_present_plan(input: &PolicyInput) -> PresentPlan {
    let display_hz = input.display.hz() as u32;
    let (game_hz, stepped) = match (input.title, input.frame_step) {
        (Some(GameRate::Display), true) => (display_hz, true),
        (Some(GameRate::Fixed { hz }), true) if hz > 0 => (hz, true),
        _ => (input.game_hz.max(1), false),
    };
    let plan = |source, reason| PresentPlan {
        display: input.display,
        game_hz,
        source,
        reason,
    };
    if game_hz == display_hz {
        return plan(
            FrameSource::PerTick,
            if stepped {
                "the frame step makes the game step at the display rate"
            } else {
                "the game's own rate is the display rate"
            },
        );
    }
    let ticks = (display_hz / game_hz).max(1);
    let divides = display_hz.is_multiple_of(game_hz);
    if !divides {
        return plan(
            FrameSource::Hold {
                ticks_per_frame: ticks,
            },
            "the game rate does not divide the display rate, so frames are shown as they come",
        );
    }
    if stepped {
        return plan(
            FrameSource::Hold {
                ticks_per_frame: ticks,
            },
            "the title steps at a fixed rate below the display rate and its frames are shown as they come",
        );
    }
    if ticks != 2 {
        return plan(
            FrameSource::Hold {
                ticks_per_frame: ticks,
            },
            "MetalFX makes one frame between two game frames, which does not fill this ratio",
        );
    }
    if !input.interpolation {
        return plan(
            FrameSource::Hold {
                ticks_per_frame: ticks,
            },
            "MetalFX frame interpolation is not available, so each game frame is shown for two ticks",
        );
    }
    plan(
        FrameSource::Interpolate,
        "the title has no frame step, so MetalFX makes the frame between two game frames",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(title: Option<GameRate>, frame_step: bool, interpolation: bool) -> PolicyInput {
        PolicyInput {
            title,
            frame_step,
            game_hz: 60,
            display: FrameInterval::Hz120,
            interpolation,
        }
    }

    #[test]
    fn a_frame_step_title_steps_per_tick() {
        let p = choose_present_plan(&input(Some(GameRate::Display), true, true));
        assert_eq!((p.source, p.game_hz), (FrameSource::PerTick, 120));
    }

    #[test]
    fn a_title_without_facts_gets_interpolation() {
        let p = choose_present_plan(&input(None, false, true));
        assert_eq!((p.source, p.game_hz), (FrameSource::Interpolate, 60));
    }

    #[test]
    fn missing_interpolation_holds_and_says_so() {
        let p = choose_present_plan(&input(Some(GameRate::Game), false, false));
        assert_eq!(p.source, FrameSource::Hold { ticks_per_frame: 2 });
        assert!(p.reason.contains("not available"));
    }

    #[test]
    fn display_pacing_without_frame_step_data_does_not_trust_the_hook() {
        let p = choose_present_plan(&input(Some(GameRate::Display), false, true));
        assert_eq!(p.source, FrameSource::Interpolate);
        assert_eq!(p.game_hz, 60);
    }

    #[test]
    fn a_thirty_hertz_game_holds_for_four_ticks() {
        let mut i = input(None, false, true);
        i.game_hz = 30;
        let p = choose_present_plan(&i);
        assert_eq!(p.source, FrameSource::Hold { ticks_per_frame: 4 });
    }

    #[test]
    fn a_rate_that_does_not_divide_holds() {
        let mut i = input(None, false, true);
        i.game_hz = 50;
        let p = choose_present_plan(&i);
        assert_eq!(p.source, FrameSource::Hold { ticks_per_frame: 2 });
        assert!(p.reason.contains("does not divide"));
    }

    #[test]
    fn a_fixed_rate_with_frame_step_is_kept() {
        let p = choose_present_plan(&input(Some(GameRate::Fixed { hz: 60 }), true, true));
        assert_eq!(p.source, FrameSource::Hold { ticks_per_frame: 2 });
        assert_eq!(p.game_hz, 60);
    }
}
