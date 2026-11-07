//! How the game's logic steps.
//!
//! [`StepMode`] says who decides the step of a frame. [`Stepper`] writes it into the state page once for each frame.
//!
//! The whole vsync step ([`StepMode::Vsyncs`]) is the plain case. The step is the vsyncs of the game that one frame covers,
//! so 1 at a target of 60 and 2 at a target of 30. A frame that takes longer than its hold slows the game, as it does on
//! the Switch.
//!
//! The load-driven step ([`StepMode::Load`]) is the step of nxnative's `fstep_tick`. It takes a longer hold when 60 does
//! not hold and one more vsync while the game is behind real time (see [`crate::load`]). The host calls
//! [`Stepper::present`] once for each presented frame, and that call writes the step.

use nxkit_abi::guest::Fault;

use crate::Error;
use crate::load::{LoadConfig, LoadStepper, Outcome, PresentSample};
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
    /// A step in whole vsyncs that the load of the frames decides, for a target of 30, 60 or 120. It runs without a state
    /// page, and then only the hold that [`Stepper::present`] returns drives the presents. At 120 the unit is 1/120 s and
    /// there is no state page.
    Load(LoadConfig),
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
            Self::Load(c) => c.target,
        }
    }
}

/// Writes the step of each frame.
#[derive(Debug)]
pub struct Stepper {
    page: Option<StatePage>,
    mode: StepMode,
    variable: VariableStep,
    load: Option<LoadStepper>,
}

fn load_of(mode: StepMode) -> Option<LoadStepper> {
    match mode {
        StepMode::Load(c) => Some(LoadStepper::new(c)),
        _ => None,
    }
}

impl Stepper {
    /// A stepper for `mode`. Writes the target into the page. A load-driven step at 120 fps has no page, so it is refused.
    pub fn new(page: StatePage, mode: StepMode) -> Result<Self, Error> {
        if matches!(mode, StepMode::Load(c) if c.at_120()) {
            return Err(Error::PageAt120);
        }
        page.set_target_fps(mode.target() as f32)?;
        Ok(Self {
            page: Some(page),
            mode,
            variable: VariableStep::new(),
            load: load_of(mode),
        })
    }

    /// A load-driven stepper without a state page. It decides the hold. A game with an engine limiter takes the target from
    /// the limiters and the present path follows the hold.
    pub fn without_page(cfg: LoadConfig) -> Self {
        let mode = StepMode::Load(cfg);
        Self {
            page: None,
            mode,
            variable: VariableStep::new(),
            load: load_of(mode),
        }
    }

    /// The mode.
    pub fn mode(&self) -> StepMode {
        self.mode
    }

    /// Whether the game fixes its own step now. It is so while the state page says a step that is not 0, as in a menu with
    /// its own frame rate. Always false without a page.
    pub fn game_fixes_step(&self) -> Result<bool, Fault> {
        match &self.page {
            Some(p) => Ok(p.own_step()? != 0),
            None => Ok(false),
        }
    }

    /// Changes the mode. The variable step and the load-driven step start again. A step in vsyncs needs a page, and a
    /// load-driven step at 120 fps needs none.
    pub fn set_mode(&mut self, mode: StepMode) -> Result<(), Error> {
        let at_120 = matches!(mode, StepMode::Load(c) if c.at_120());
        match (&self.page, mode) {
            (None, StepMode::Own | StepMode::Load(_)) => {}
            (None, _) => return Err(Error::NoStatePage),
            (Some(_), _) if at_120 => return Err(Error::PageAt120),
            (Some(p), _) => {
                p.set_target_fps(mode.target() as f32)?;
                p.clear_variable_step()?;
            }
        }
        self.variable.reset();
        self.load = load_of(mode);
        self.mode = mode;
        Ok(())
    }

    /// A game frame starts at `start_ns`, a monotonic time in nanoseconds. Writes its step.
    ///
    /// While the game steps by itself (`OWN_STEP` is not zero, for example in a menu with its own frame rate) the
    /// variable step is cleared and starts again with the next frame.
    pub fn frame_started(&mut self, start_ns: u64) -> Result<(), Fault> {
        self.frame_started_waited(start_ns, 0)
    }

    /// The same, for a frame whose first acquire waited `wait_ns` for the pacing. The load-driven step takes that wait off
    /// the cost of the frame.
    pub fn frame_started_waited(&mut self, start_ns: u64, wait_ns: u64) -> Result<(), Fault> {
        let Some(page) = &self.page else {
            if let Some(l) = &mut self.load {
                l.frame_started(start_ns, wait_ns);
            }
            return Ok(());
        };
        match self.mode {
            StepMode::Own => Ok(()),
            StepMode::Vsyncs(fps) => page.set_step_vsyncs(60 / fps),
            StepMode::Load(_) => {
                if let Some(l) = &mut self.load {
                    l.frame_started(start_ns, wait_ns);
                }
                Ok(())
            }
            StepMode::Variable(cap) => {
                if page.own_step()? != 0 {
                    self.variable.reset();
                    return page.clear_variable_step();
                }
                let step = self.variable.next(start_ns, cap);
                page.set_variable_step(step)
            }
        }
    }

    /// A frame is presented. Runs the load-driven step and writes its step. `None` in the other modes.
    pub fn present(&mut self, sample: &PresentSample) -> Result<Option<Outcome>, Fault> {
        match &mut self.load {
            Some(l) => l.present(sample, self.page.as_ref()).map(Some),
            None => Ok(None),
        }
    }

    /// The load-driven stepper, for its counters.
    pub fn load(&self) -> Option<&LoadStepper> {
        self.load.as_ref()
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
        assert_eq!(s.game_fixes_step(), Ok(false));
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

    #[test]
    fn a_load_step_takes_a_page_below_120_and_none_at_120() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut mem = vec![0u64; 0x60 / 8 + 2];
        let base = mem.as_mut_ptr() as u64;
        guest::set_range(base, (mem.len() * 8) as u64);
        let Ok(page) = StatePage::new(base) else {
            return;
        };
        let Ok(c60) = LoadConfig::new(60) else {
            return;
        };
        let Ok(c120) = LoadConfig::new(120) else {
            return;
        };
        assert!(matches!(
            Stepper::new(page, StepMode::Load(c120)),
            Err(Error::PageAt120)
        ));
        let Ok(page) = StatePage::new(base) else {
            return;
        };
        let Ok(mut s) = Stepper::new(page, StepMode::Load(c60)) else {
            return;
        };
        assert_eq!(guest::read_f32(base).ok(), Some(60.0));
        assert_eq!(s.set_mode(StepMode::Load(c120)), Err(Error::PageAt120));
        let mut free = Stepper::without_page(c120);
        assert_eq!(free.set_mode(StepMode::Vsyncs(60)), Err(Error::NoStatePage));
        assert!(free.set_mode(StepMode::Own).is_ok());
        assert!(free.frame_started(1).is_ok());
    }
}
