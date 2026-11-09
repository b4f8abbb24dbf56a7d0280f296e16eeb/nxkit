//! The variable frame step above 60 fps.
//!
//! A game whose frame step data allows it (Breath of the Wild) steps its logic by a time that the host writes into the
//! state page for each frame. The time is the time since the previous frame started, in units of 1/30 s. At 120 fps
//! on a 120 Hz display a step is 0.25.
//!
//! The step has a floor, because a very short step makes the game's physics unstable. A step under the floor is raised
//! to it, and the time that this adds is a carry that the next steps give back. Without the carry the game runs fast
//! on a display whose rate is not a multiple of the cap. A step over the ceiling is cut to it, so the game runs slower
//! than real time when frames take longer than the ceiling, as it does on the Switch.
//!
//! By default the time is the time between the starts of two frames (the first acquire of each). On a screen that
//! adapts its refresh rate and shows each frame as soon as it is ready, the host marks the time the display showed each
//! frame ([`VariableStep::note_shown`]), and the step is the time between two shown frames, so game time follows what
//! the screen shows.
//!
//! The logic is ported from `dm/gfx/nvn/nvn_fstep.h` (`nvn_vstep_next`)
//! `0f2a772c13e2caed2959d1d1a93f8fe57f44c754`. It is listed in `THIRD_PARTY.md`.

/// The smallest step in 1/30 s units, one 240th of a second.
pub const STEP_MIN: f32 = 0.125;
/// The largest step in 1/30 s units, the longest step of the game itself.
pub const STEP_MAX: f32 = 1.5;
/// Nanoseconds to units of 1/30 s.
const NS_TO_UNITS: f64 = 30e-9;

/// The state of the variable frame step.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VariableStep {
    prev_ns: u64,
    carry: f32,
    /// The step that the last shown frame gave, in 1/30 s units. 0 until a frame is shown.
    shown_step: f32,
}

impl VariableStep {
    /// A step that starts from the next frame.
    pub const fn new() -> Self {
        Self {
            prev_ns: 0,
            carry: 0.0,
            shown_step: 0.0,
        }
    }

    /// Forgets the previous frame. Call it when the game's own step ran, so the next frame takes one frame at `cap`.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The step of the frame that starts at `start_ns`, in 1/30 s units. `cap_fps` is the target frame rate, 61 to
    /// 240. A zero `cap_fps` is taken as 60.
    pub fn next(&mut self, start_ns: u64, cap_fps: u32) -> f32 {
        if self.shown_step > 0.0 {
            return self.shown_step;
        }
        self.advance(start_ns, cap_fps)
    }

    /// A frame was shown at `shown_ns` (the same clock as the starts of frames). From now on [`VariableStep::next`] gives
    /// the time between the last two shown frames, with the floor and its carry applied once for each shown frame.
    /// A frame that starts before the previous one was shown gets the step of the last shown frame again.
    pub fn note_shown(&mut self, shown_ns: u64, cap_fps: u32) {
        self.shown_step = self.advance(shown_ns, cap_fps);
    }

    fn advance(&mut self, start_ns: u64, cap_fps: u32) -> f32 {
        let cap = if cap_fps == 0 { 60 } else { cap_fps };
        let mut step = if self.prev_ns != 0 && start_ns > self.prev_ns {
            ((start_ns - self.prev_ns) as f64 * NS_TO_UNITS) as f32 + self.carry
        } else {
            30.0 / cap as f32
        };
        self.prev_ns = start_ns;
        if step < STEP_MIN {
            self.carry = (step - STEP_MIN).max(-STEP_MAX);
            step = STEP_MIN;
        } else {
            self.carry = 0.0;
        }
        step.min(STEP_MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cap: u32, intervals_ms: &[f64]) -> (f64, f64, f32) {
        let mut v = VariableStep::new();
        let mut t = 1_000_000_000u64;
        let mut game = 0.0;
        let mut real = 0.0;
        let mut least = f32::MAX;
        let _ = v.next(t, cap);
        for i in 0..2000 {
            let d = intervals_ms[i % intervals_ms.len()];
            t += (d * 1e6) as u64;
            let s = v.next(t, cap);
            game += f64::from(s) / 30.0;
            real += d / 1000.0;
            least = least.min(s);
        }
        (game, real, least)
    }

    #[test]
    fn the_first_frame_takes_one_frame_at_the_cap() {
        let mut v = VariableStep::new();
        assert_eq!(v.next(5, 120), 0.25);
    }

    #[test]
    fn a_steady_120_fps_steps_a_quarter() {
        let mut v = VariableStep::new();
        let _ = v.next(1_000_000_000, 120);
        let s = v.next(1_000_000_000 + 8_333_333, 120);
        assert!((s - 0.25).abs() < 1e-3, "{s}");
    }

    #[test]
    fn the_game_keeps_real_time_when_ticks_lie_unevenly() {
        // 240 fps on a 360 Hz display: ticks lie 2.78 ms and 5.56 ms apart in turn.
        let (game, real, least) = run(240, &[2.7778, 5.5556]);
        assert!((game / real - 1.0).abs() < 0.002, "{}", game / real);
        assert!(least >= STEP_MIN);
    }

    #[test]
    fn a_slow_frame_slows_the_game() {
        let mut v = VariableStep::new();
        let _ = v.next(1_000_000_000, 120);
        let s = v.next(1_000_000_000 + 100_000_000, 120);
        assert_eq!(s, STEP_MAX);
    }

    #[test]
    fn shown_frames_give_the_step_and_starts_are_ignored() {
        let mut v = VariableStep::new();
        // Before a frame is shown the step comes from the starts of frames.
        let _ = v.next(1_000_000_000, 120);
        let s = v.next(1_000_000_000 + 16_666_667, 120);
        assert!((s - 0.5).abs() < 1e-3, "{s}");
        let mut v = VariableStep::new();
        v.note_shown(2_000_000_000, 120);
        // 13 ms between two shown frames, whatever the starts say.
        v.note_shown(2_000_000_000 + 13_000_000, 120);
        let s = v.next(5, 120);
        assert!((s - 0.39).abs() < 1e-3, "{s}");
        assert_eq!(v.next(9_000_000_000, 120), s);
    }

    #[test]
    fn shown_frames_keep_real_time_across_the_floor() {
        // 3 ms and 7 ms in turn: the 3 ms steps are raised to the floor and the carry pays them back.
        let mut v = VariableStep::new();
        let mut t = 1_000_000_000u64;
        v.note_shown(t, 120);
        let (mut game, mut real) = (0.0, 0.0);
        for i in 0..2000 {
            let d = if i % 2 == 0 { 3.0 } else { 7.0 };
            t += (d * 1e6) as u64;
            v.note_shown(t, 120);
            game += f64::from(v.next(0, 120)) / 30.0;
            real += d / 1000.0;
        }
        assert!((game / real - 1.0).abs() < 0.002, "{}", game / real);
    }

    #[test]
    fn a_reset_starts_again() {
        let mut v = VariableStep::new();
        let _ = v.next(1_000_000_000, 120);
        v.reset();
        assert_eq!(v, VariableStep::new());
    }
}
