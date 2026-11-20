//! The target frame rate of a display link and the range of rates it may run at. The pacer, its policy and the metrics share it, so it sits below them.

use objc2_quartz_core::CAFrameRateRange;

/// The target frame rate of a [`DisplayLink`](crate::DisplayLink).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameInterval {
    /// 60 frames per second.
    Hz60,
    /// 120 frames per second.
    Hz120,
    /// Any other whole rate, for example 72 or 144. [`FrameInterval::from_hz`] makes it.
    Hz(u32),
}

impl FrameInterval {
    /// The interval for a rate in Hz, at least 1.
    pub fn from_hz(hz: u32) -> Self {
        match hz {
            60 => FrameInterval::Hz60,
            120 => FrameInterval::Hz120,
            other => FrameInterval::Hz(other.max(1)),
        }
    }

    /// The frames per second.
    pub fn hz(self) -> f32 {
        match self {
            FrameInterval::Hz60 => 60.0,
            FrameInterval::Hz120 => 120.0,
            FrameInterval::Hz(hz) => hz as f32,
        }
    }

    /// The range of a fixed rate: minimum, maximum and preferred are the same.
    pub fn range(self) -> RateRange {
        let hz = self.hz();
        RateRange {
            min: hz,
            max: hz,
            preferred: hz,
        }
    }
}

/// The range of rates that the display link may run at, in frames per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateRange {
    /// The lowest rate the game needs.
    pub min: f32,
    /// The highest rate, usually the highest rate of the screen.
    pub max: f32,
    /// The rate the link asks for.
    pub preferred: f32,
}

impl RateRange {
    /// The range for variable refresh: from `floor_hz` (what the game needs at least) to `screen_max_hz`, with `target_hz`
    /// preferred. The target is kept inside the screen's rates, and the floor never goes above the target.
    pub fn plan(floor_hz: u32, screen_max_hz: u32, target_hz: u32) -> Self {
        let max = screen_max_hz.max(1);
        let preferred = target_hz.clamp(1, max);
        let min = floor_hz.clamp(1, preferred);
        Self {
            min: min as f32,
            max: max as f32,
            preferred: preferred as f32,
        }
    }

    /// Whether the range is wider than one rate.
    pub fn is_variable(&self) -> bool {
        self.max > self.min
    }

    pub(crate) fn to_ca(self) -> CAFrameRateRange {
        CAFrameRateRange {
            minimum: self.min,
            maximum: self.max,
            preferred: self.preferred,
        }
    }
}
