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
//! The logic is ported from `dm/gfx/nvn/nvn_fstep.h` of nxnative (`nvn_vstep_next`) at the head of pull request 76,
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
}

impl VariableStep {
    /// A step that starts from the next frame.
    pub const fn new() -> Self {
        Self {
            prev_ns: 0,
            carry: 0.0,
        }
    }

    /// Forgets the previous frame. Call it when the game's own step ran, so the next frame takes one frame at `cap`.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The step of the frame that starts at `start_ns`, in 1/30 s units. `cap_fps` is the target frame rate, 61 to
    /// 240. A zero `cap_fps` is taken as 60.
    pub fn next(&mut self, start_ns: u64, cap_fps: u32) -> f32 {
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
        let mut i = 0;
        let _ = v.next(t, cap);
        for _ in 0..2000 {
            let d = intervals_ms[i % intervals_ms.len()];
            i += 1;
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
    fn a_reset_starts_again() {
        let mut v = VariableStep::new();
        let _ = v.next(1_000_000_000, 120);
        v.reset();
        assert_eq!(v, VariableStep::new());
    }
}
