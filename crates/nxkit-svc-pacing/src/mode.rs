//! How the game's logic steps.
//!
//! [`StepMode`] says who decides the step of a frame. [`Stepper`] writes it into the state page once for each frame.
//!
//! The whole vsync step is the plain case. The step is the vsyncs of the game that one frame covers, so 1 at a target of
//! 60 and 2 at a target of 30. The step that nxnative's `fstep_tick` takes from the load of the GPU (a longer hold when
//! 60 does not hold, one more vsync while the game is behind real time) is not ported. A frame that takes longer than
//! its hold slows the game, as it does on the Switch.

use nxkit_abi::guest::Fault;

use crate::state::StatePage;
use crate::vstep::VariableStep;

/// The highest target of a variable step.
pub const MAX_VARIABLE_FPS: u32 = 240;

/// Who decides the step of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepMode {
    /// The game's own step. The state page says 0.
    Own,
    /// A step in whole vsyncs for a target of 30 or 60.
    Vsyncs(u32),
    /// A step in 1/30 s units from the time between frames, for a target of 61 to 240.
    Variable(u32),
}

/// An invalid target frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadTarget(pub u32);

impl std::fmt::Display for BadTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a frame step target is 0, 30, 60 or 61 to {MAX_VARIABLE_FPS}, not {}",
            self.0
        )
    }
}

impl std::error::Error for BadTarget {}

impl StepMode {
    /// The mode for a target frame rate. 0 is the game's own step, 30 and 60 are whole vsyncs and 61 to 240 are variable.
    pub fn for_target(fps: u32) -> Result<Self, BadTarget> {
        match fps {
            0 => Ok(Self::Own),
            30 | 60 => Ok(Self::Vsyncs(fps)),
            61..=MAX_VARIABLE_FPS => Ok(Self::Variable(fps)),
            other => Err(BadTarget(other)),
        }
    }

    /// The target frame rate that goes in the state page.
    pub fn target(self) -> u32 {
        match self {
            Self::Own => 0,
            Self::Vsyncs(f) | Self::Variable(f) => f,
        }
    }
}

/// Writes the step of each frame.
#[derive(Debug)]
pub struct Stepper {
    page: StatePage,
    mode: StepMode,
    variable: VariableStep,
}

impl Stepper {
    /// A stepper for `mode`. Writes the target into the page.
    pub fn new(page: StatePage, mode: StepMode) -> Result<Self, Fault> {
        page.set_target_fps(mode.target() as f32)?;
        Ok(Self {
            page,
            mode,
            variable: VariableStep::new(),
        })
    }

    /// The mode.
    pub fn mode(&self) -> StepMode {
        self.mode
    }

    /// Changes the mode. The variable step starts again.
    pub fn set_mode(&mut self, mode: StepMode) -> Result<(), Fault> {
        self.page.set_target_fps(mode.target() as f32)?;
        self.page.clear_variable_step()?;
        self.variable.reset();
        self.mode = mode;
        Ok(())
    }

    /// A game frame starts at `start_ns`, a monotonic time in nanoseconds. Writes its step.
    ///
    /// While the game steps by itself (`OWN_STEP` is not zero, for example in a menu with its own frame rate) the
    /// variable step is cleared and starts again with the next frame.
    pub fn frame_started(&mut self, start_ns: u64) -> Result<(), Fault> {
        match self.mode {
            StepMode::Own => Ok(()),
            StepMode::Vsyncs(fps) => self.page.set_step_vsyncs(60 / fps),
            StepMode::Variable(cap) => {
                if self.page.own_step()? != 0 {
                    self.variable.reset();
                    return self.page.clear_variable_step();
                }
                let step = self.variable.next(start_ns, cap);
                self.page.set_variable_step(step)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_abi::guest;

    #[test]
    fn a_target_picks_its_mode() {
        assert_eq!(StepMode::for_target(0), Ok(StepMode::Own));
        assert_eq!(StepMode::for_target(30), Ok(StepMode::Vsyncs(30)));
        assert_eq!(StepMode::for_target(60), Ok(StepMode::Vsyncs(60)));
        assert_eq!(StepMode::for_target(120), Ok(StepMode::Variable(120)));
        assert_eq!(StepMode::for_target(240), Ok(StepMode::Variable(240)));
        assert_eq!(StepMode::for_target(45), Err(BadTarget(45)));
        assert_eq!(StepMode::for_target(241), Err(BadTarget(241)));
    }

    #[test]
    fn the_stepper_writes_each_mode() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut mem = vec![0u64; 0x60 / 8 + 2];
        let base = mem.as_mut_ptr() as u64;
        guest::set_range(base, (mem.len() * 8) as u64);
        let Ok(page) = StatePage::new(base) else {
            return;
        };
        let Ok(mut s) = Stepper::new(page, StepMode::Vsyncs(30)) else {
            return;
        };
        assert_eq!(guest::read_f32(base).ok(), Some(30.0));
        assert!(s.frame_started(1).is_ok());
        assert_eq!(guest::read_u32(base + 0x20).ok(), Some(2));

        assert!(s.set_mode(StepMode::Variable(120)).is_ok());
        assert!(s.frame_started(1_000_000_000).is_ok());
        assert_eq!(guest::read_f32(base + 0x50).ok(), Some(0.25));
        assert!(s.frame_started(1_000_000_000 + 8_333_333).is_ok());
        let step = guest::read_f32(base + 0x50).unwrap_or(0.0);
        assert!((step - 0.25).abs() < 1e-3);

        // The game's own step wins and the variable step starts again.
        assert!(guest::write_u32(base + 0x24, 2).is_ok());
        assert!(s.frame_started(2_000_000_000).is_ok());
        assert_eq!(guest::read_f32(base + 0x50).ok(), Some(0.0));
        assert!(guest::write_u32(base + 0x24, 0).is_ok());
        assert!(s.frame_started(2_100_000_000).is_ok());
        assert_eq!(guest::read_f32(base + 0x50).ok(), Some(0.25));
    }
}
