//! The inputs and outputs of the load-driven frame step.

use crate::mode::BadTarget;
use crate::watch::PerfCounters;

/// How the load-driven step runs. The title's facts and the platform set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadConfig {
    /// The target frame rate: 30, 60 or 120. At 120 the unit of the step is a tick of 1/120 s and there is no state page.
    pub target: u32,
    /// Whether late frames may raise the hold above the cap's vsyncs (`fps_hold` of the profile). It is off for a game whose
    /// updates do not wait for the presents and step by the cap whatever the hold (Age of Calamity). Each frame is then
    /// shown as soon as it is ready.
    pub hold: bool,
    /// Whether the thermal cap works. A system without a thermal state never reaches it.
    pub thermal_cap: bool,
    /// Whether the grace of a probe is renewed while its frames keep getting cheaper (macOS raises the GPU clock over
    /// seconds).
    pub probe_ramp: bool,
    /// The core types of the system (`hw.nperflevels`). One never reports a slow CPU.
    pub perf_levels: u32,
    /// Only the work of a frame judges it ([`crate::fstep::FrameStep::work`], `game.json` key `pace_work`).
    pub work: bool,
    /// The step never leaves the cap (`game.json` `pace_profile` `steady`): no hold above the cap's vsyncs and no thermal
    /// cap; a movie still caps it. A frame that runs long shows late once and the game pays the time back. For a game
    /// that must keep its frame rate whatever a frame costs (Ocarina of Time fell to 30 fps for short hitches).
    pub steady: bool,
}

impl LoadConfig {
    /// A config for a target of 30, 60 or 120 with a hold, the thermal cap and the probe ramp on.
    pub fn new(target: u32) -> Result<Self, BadTarget> {
        match target {
            30 | 60 | 120 => Ok(Self {
                target,
                hold: true,
                thermal_cap: true,
                probe_ramp: true,
                perf_levels: 1,
                work: false,
                steady: false,
            }),
            other => Err(BadTarget(other)),
        }
    }

    /// Whether the unit of the step is a tick of 1/120 s.
    pub fn at_120(&self) -> bool {
        self.target == 120
    }
}

/// What the screen shows of an earlier frame, when present snapshots are on. The game runs up to a frame ahead of the
/// screen then, and a frame that runs long is absorbed, so only what the screen shows says whether a frame was late.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapView {
    /// The time in seconds at which the present 8 frames back was shown. Negative when it was dropped, 0 when unknown.
    pub presented: f64,
    /// The same for the present before it.
    pub before: f64,
    /// The milliseconds that the present 8 frames back was to stay on screen.
    pub expect_ms: f64,
}

/// What the host knows at a present.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresentSample {
    /// The presents so far, counted from 1.
    pub presents: u64,
    /// The time now in nanoseconds, monotonic.
    pub now_ns: u64,
    /// The GPU time in milliseconds of the latest finished frame among the last eight, when there is one.
    pub gpu_ms: Option<f64>,
    /// The view of the screen. `Some` only when present snapshots are on, the link is active and not stalled. The frames
    /// that the stall fallback drops say nothing about the game.
    pub snap: Option<SnapView>,
    /// The highest refresh rate of the screen in Hz, 0 without a window.
    pub screen_hz: u32,
    /// Whether the display link drives the presents.
    pub link_active: bool,
    /// Whether a prerendered movie plays. The movie is paced for the Switch's 30 and 60.
    pub movie: bool,
    /// The thermal state of the system, 0 nominal to 3 critical. A Mac gives 0.
    pub thermal: i32,
    /// Whether Low Power Mode is on.
    pub low_power: bool,
    /// The CPU time counters, read in the presents that are a multiple of 30, when the system gives them.
    pub perf: Option<PerfCounters>,
}

/// Why the cap is what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapWhy {
    /// The target.
    Target,
    /// A movie plays.
    Movie,
    /// The thermal state is serious.
    Thermal,
    /// The screen shows less than 120 Hz.
    ScreenBelow120,
}

/// Why the hold changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoldWhy {
    /// The game fixes its own step.
    GameStep,
    /// The game's own step ended.
    GameStepEnded,
    /// The cap changed.
    CapChanged,
    /// Three late frames in a row.
    ThreeLate,
    /// A probe back to the shortest hold, with what started it.
    Probe {
        /// The share of the CPU time on performance cores in percent, when the CPU is slow.
        cpu_share: Option<i32>,
        /// The GPU time at the longer hold against its time at the shortest hold, when the GPU is slow.
        gpu_ratio: Option<f64>,
        /// The cost at the longer hold against the cost that held the shortest, when it is the same scene.
        same_scene: Option<f64>,
    },
    /// A trial of the shorter hold after 30 frames that fit it.
    Trial,
}

/// A change that the engine writes to its log. The host logs it.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The cap changed.
    CapChanged {
        /// The cap before in fps, 0 at the first present.
        from: u32,
        /// The cap now.
        to: u32,
        /// The reason.
        why: CapWhy,
    },
    /// The hold changed.
    HoldChanged {
        /// The hold before.
        from: i32,
        /// The hold now.
        to: i32,
        /// The reason.
        why: HoldWhy,
        /// The cost of the frame in vsyncs.
        cost: f64,
        /// The GPU time of the frame in vsyncs.
        gpu: f64,
        /// At a step up: the cost that held the shortest hold, when it was learned.
        held_at: Option<f64>,
    },
    /// The game fixed its step (`vsyncs` above 0) or gave it back to the host (0).
    GameStep {
        /// The step in vsyncs.
        vsyncs: i32,
    },
    /// A probe ended. It held, or it failed.
    ProbeEnded {
        /// Whether the probe held.
        held: bool,
        /// The CPU share in percent, when measured.
        cpu_share: Option<i32>,
    },
    /// The run at the shortest hold after a probe was too short and counts as a failed probe.
    ShortRun {
        /// The shortest hold.
        kmin: i32,
        /// The CPU share in percent, when measured.
        cpu_share: Option<i32>,
    },
    /// The summary of 1800 presents.
    Window {
        /// The frames that were held for 1, 2 and 3 vsyncs.
        held: [u64; 3],
        /// The seconds of game time and of real time, from the state page. `None` at 120 fps, which has no game clock to
        /// read, and in the first window.
        seconds: Option<(f64, f64)>,
    },
}

/// What a present decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The cap in fps.
    pub cap: u32,
    /// The vsyncs that the frame stays on screen, in units of 1/60 s (1/120 s at 120 fps).
    pub hold: i32,
    /// The step handed to the game for the next frame.
    pub step: i32,
    /// The shortest hold of the cap.
    pub kmin: i32,
    /// What changed.
    pub events: Vec<Event>,
}

/// What the frame step counted.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoadStats {
    /// Late frames.
    pub late: u64,
    /// Vsyncs of debt that were paid back with an extra vsync.
    pub paid: u64,
    /// Steps up of the hold.
    pub ups: u64,
    /// Steps down of the hold.
    pub downs: u64,
    /// Trials that failed.
    pub failed: u64,
    /// Probes.
    pub probes: u64,
    /// Vsyncs of debt dropped after a long stall.
    pub dropped: f64,
    /// Real time that the game is behind, in vsyncs.
    pub debt: f64,
    /// Frames held for 1, 2 and 3 vsyncs since the last summary.
    pub held: [u64; 3],
}
