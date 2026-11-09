//! The load-driven frame step: the port of `fstep_tick` (R30a).
//!
//! The host calls [`LoadStepper::present`] once for each presented frame, before the period of the frame is taken. The time
//! since the last frame's start (`shown`) and the cost of the frame decide how long the frame stays on screen (the hold)
//! and the step of the next frame ([`crate::fstep`]).
//!
//! The cost is the larger of two numbers, both in vsyncs. The CPU time is the time between the frames' starts without
//! the pacing wait. The GPU time is the one of the latest finished frame, taken from the last eight frames. A GPU that
//! cannot keep up makes frames late too.
//!
//! The longer hold and the extra vsync of a late frame come from [`crate::fstep::FrameStep`]. This module feeds it:
//!
//! - the cap, which is 30 while a movie plays, under the thermal cap and at a target of 30, else 60 (at a target of 120 the
//!   cap is 120 or 60),
//! - the longest hold, which follows the title's `fps_hold`,
//! - the step that the game fixes itself, which the host reads from the state page,
//! - the verdict of the screen when present snapshots are on,
//! - the reports that the CPU or the GPU runs slow for a reason other than heat.
//!
//! Source: `fstep_tick`, `fps_step_cap`, `high_cap_tick` and the present part of `nvnQueuePresentTexture` in
//! `dm/gfx/nvn/hle_nvn.c`, read on 2026-11-07 at `88c7e03f`. The thresholds are own.

use nxkit_abi::guest::Fault;

use crate::fstep::{FSTEP_LATE, FSTEP_MAX, FrameInput, FrameStep, GpuSlow, Screen};
use crate::state::StatePage;
use crate::watch::{CpuWatch, ThermalCap};

/// The presents between two lines of the summary (30 s at 60 fps).
const WINDOW_PRESENTS: u64 = 1800;
/// The changes of the hold that [`Event`]s report. Later ones are only counted.
const EVENT_LIMIT: u32 = 40;
/// The presents that must pass before the screen's verdict counts.
const SNAP_AFTER: u64 = 9;
/// A frame that was on screen this many times longer than expected is late.
const SNAP_LATE: f64 = 1.25;
/// Late frames among the last 16 that make the game late too.
const SLOW_FRAMES: u32 = 12;

/// The state of the load-driven frame step.
#[derive(Debug)]
pub struct LoadStepper {
    cfg: LoadConfig,
    fs: FrameStep,
    gpu: GpuSlow,
    cpu: CpuWatch,
    thermal: ThermalCap,
    start_ns: u64,
    wait_ns: u64,
    prev_ns: u64,
    slow_bits: u16,
    slow_k: i32,
    cap_was: u32,
    held: [u64; 3],
    fixed_sum: u64,
    fixed_was: i32,
    nlog: u32,
    window_ns: u64,
    window_vsyncs: u64,
}

impl LoadStepper {
    /// A stepper for `cfg`.
    pub fn new(cfg: LoadConfig) -> Self {
        Self {
            cfg,
            fs: {
                let mut fs = FrameStep::default();
                fs.work = cfg.work;
                fs.drop_debt = cfg.steady;
                fs
            },
            gpu: GpuSlow::default(),
            cpu: CpuWatch::new(cfg.perf_levels),
            thermal: ThermalCap::new(cfg.thermal_cap),
            start_ns: 0,
            wait_ns: 0,
            prev_ns: 0,
            slow_bits: 0,
            slow_k: 0,
            cap_was: 0,
            held: [0; 3],
            fixed_sum: 0,
            fixed_was: 0,
            nlog: 0,
            window_ns: 0,
            window_vsyncs: 0,
        }
    }

    /// The config.
    pub fn config(&self) -> LoadConfig {
        self.cfg
    }

    /// The hold that presentation follows, 0 while nothing is decided.
    pub fn hold(&self) -> i32 {
        self.fs.k
    }

    /// The first acquire after a present returned: the start of the frame, and the pacing wait that it did. Later calls in
    /// the same frame change nothing.
    pub fn frame_started(&mut self, start_ns: u64, pace_wait_ns: u64) {
        if self.start_ns == 0 {
            self.start_ns = start_ns;
            self.wait_ns = pace_wait_ns;
        }
    }

    /// What the frame step counted.
    pub fn stats(&self) -> LoadStats {
        LoadStats {
            late: self.fs.late,
            paid: self.fs.paid,
            ups: self.fs.ups,
            downs: self.fs.downs,
            failed: self.fs.failed,
            probes: self.fs.probes,
            dropped: self.fs.dropped,
            debt: self.fs.debt,
            held: self.held,
        }
    }

    fn cap(&mut self, s: &PresentSample) -> (u32, CapWhy) {
        // A steady step keeps the target in any thermal state. A movie still caps it: the game asks for that.
        let thermal = self.thermal.capped(s.now_ns, s.thermal) && !self.cfg.steady;
        if self.cfg.at_120() {
            let screen_ok = s.screen_hz == 0 || s.screen_hz >= 120;
            return match (screen_ok, s.movie, thermal) {
                (true, false, false) => (120, CapWhy::Target),
                (false, _, _) => (60, CapWhy::ScreenBelow120),
                (_, true, _) => (60, CapWhy::Movie),
                _ => (60, CapWhy::Thermal),
            };
        }
        if s.movie {
            (30, CapWhy::Movie)
        } else if self.cfg.target <= 30 {
            (30, CapWhy::Target)
        } else if thermal {
            (30, CapWhy::Thermal)
        } else {
            (60, CapWhy::Target)
        }
    }

    fn judge_screen(&mut self, s: &PresentSample, shown: f64, kmin: i32) {
        let Some(v) = s.snap.filter(|_| s.presents > SNAP_AFTER) else {
            return;
        };
        if v.presented < 0.0 {
            self.fs.screen = Screen::Late;
        } else if v.presented > 0.0 && v.before > 0.0 {
            let late = (v.presented - v.before) * 1e3 > SNAP_LATE * v.expect_ms;
            self.fs.screen = if late { Screen::Late } else { Screen::OnTime };
        }
        // A game that is slower than its hold frame after frame is late too, though the screen misses only every few
        // frames. 18.9 ms frames at hold 1 missed one refresh in 7 and never three in a row, and ran at a jittering 53 fps
        // with a payback every few frames instead of a steady 30.
        let k = if self.fs.k != 0 { self.fs.k } else { kmin };
        if k != self.slow_k {
            self.slow_k = k;
            self.slow_bits = 0;
        }
        let slow = shown > f64::from(k) + FSTEP_LATE;
        self.slow_bits = self.slow_bits << 1 | u16::from(slow);
        if self.slow_bits.count_ones() >= SLOW_FRAMES {
            self.fs.screen = Screen::Late;
        }
    }

    /// Runs once a present, before the period of the frame is taken. Writes the step into the state page when there is
    /// one.
    pub fn present(
        &mut self,
        s: &PresentSample,
        page: Option<&StatePage>,
    ) -> Result<Outcome, Fault> {
        let mut events = Vec::new();
        if let Some(p) = page {
            p.set_movie_playing(s.movie)?;
        }
        let (cap, why) = self.cap(s);
        if cap != self.cap_was {
            events.push(Event::CapChanged {
                from: self.cap_was,
                to: cap,
                why,
            });
            self.cap_was = cap;
        }
        let hz: i32 = if self.cfg.at_120() { 120 } else { 60 };
        let v = 1e9 / f64::from(hz);
        let start = if self.start_ns != 0 {
            self.start_ns
        } else {
            s.now_ns
        };
        let pace_wait = self.wait_ns;
        self.start_ns = 0;
        self.wait_ns = 0;
        let shown = if self.prev_ns != 0 {
            start.saturating_sub(self.prev_ns) as f64 / v
        } else {
            0.0
        };
        self.prev_ns = start;
        let gpu = s.gpu_ms.filter(|g| *g >= 0.0).map_or(0.0, |g| g * 1e6 / v);
        let cost = (shown - pace_wait as f64 / v).max(gpu);
        cost_diag(
            shown,
            pace_wait as f64 / v,
            gpu,
            &self.fs,
            kmax_diag(&self.cfg, hz, cap),
        );
        let kmin = hz / cap as i32;
        let kmax = if self.cfg.steady {
            kmin
        } else if self.cfg.at_120() {
            2
        } else if !self.cfg.hold {
            kmin
        } else if kmin >= 2 {
            2
        } else {
            FSTEP_MAX
        };
        let (k0, kmin0, probe0) = (self.fs.k, self.fs.kmin, self.fs.probe);
        let fixed = match page {
            Some(p) => i32::try_from(p.own_step()?).unwrap_or(i32::MAX),
            None => 0,
        };
        let (probes0, failed0) = (self.fs.probes, self.fs.failed);
        // A screen whose rate is not a multiple of the step rate shows the link's ticks on their nearest refreshes, so a
        // frame on time can show for one refresh more than its hold.
        let screen_hz = i64::from(s.screen_hz);
        self.fs.shown_slack =
            if screen_hz > i64::from(hz) && screen_hz % i64::from(hz) != 0 && s.link_active {
                f64::from(hz) / screen_hz as f64
            } else {
                0.0
            };
        let cslow = self.cpu.slowed(s.presents, s.perf, s.thermal, s.low_power);
        let gslow = self
            .gpu
            .slowed(gpu, if k0 != 0 { k0 } else { kmin }, kmin, probe0)
            && s.thermal <= 1
            && !s.low_power;
        self.judge_screen(s, shown, kmin);
        self.fs.ramp = self.cfg.probe_ramp;
        self.fs.per_vsync = hz / 60;
        let step = self.fs.next(&FrameInput {
            shown,
            cost,
            kmin,
            kmax,
            fixed,
            slow: cslow || gslow,
        });
        if let Some(p) = page {
            p.set_step_vsyncs(u32::try_from(step).unwrap_or(1))?;
        }
        if let Some(h) = usize::try_from(self.fs.k - 1)
            .ok()
            .and_then(|i| self.held.get_mut(i))
        {
            *h += 1;
        }
        if fixed != 0 {
            self.fixed_sum += u64::try_from(step).unwrap_or(0);
        }
        if fixed != self.fixed_was && self.nlog < EVENT_LIMIT {
            events.push(Event::GameStep { vsyncs: fixed });
        }
        self.fixed_was = fixed;
        self.report(
            s,
            &mut events,
            page,
            Change {
                k0,
                kmin,
                kmin0,
                probe0,
                probes0,
                failed0,
                fixed,
                cost,
                gpu,
                cslow,
                gslow,
            },
        )?;
        Ok(Outcome {
            cap,
            hold: self.fs.k,
            step,
            kmin,
            events,
        })
    }

    fn report(
        &mut self,
        s: &PresentSample,
        events: &mut Vec<Event>,
        page: Option<&StatePage>,
        c: Change,
    ) -> Result<(), Fault> {
        let fs = &self.fs;
        if c.k0 != 0 && fs.k != c.k0 && self.nlog < EVENT_LIMIT {
            let why = if c.fixed != 0 {
                HoldWhy::GameStep
            } else if c.kmin0 > FSTEP_MAX {
                HoldWhy::GameStepEnded
            } else if c.kmin != c.kmin0 {
                HoldWhy::CapChanged
            } else if fs.k > c.k0 {
                HoldWhy::ThreeLate
            } else if fs.probes != c.probes0 {
                HoldWhy::Probe {
                    cpu_share: c.cslow.then(|| self.cpu.share()).flatten(),
                    gpu_ratio: c.gslow.then(|| self.gpu.ratio()),
                    same_scene: fs.same_scene().then(|| fs.ccur / fs.cref),
                }
            } else {
                HoldWhy::Trial
            };
            events.push(Event::HoldChanged {
                from: c.k0,
                to: fs.k,
                why,
                cost: c.cost,
                gpu: c.gpu,
                held_at: (fs.k > c.k0 && fs.cref > 0.0).then_some(fs.cref),
            });
            self.nlog += 1;
        }
        if self.nlog < EVENT_LIMIT && c.fixed == 0 {
            if c.probe0 == 1 && fs.probe != 1 {
                events.push(Event::ProbeEnded {
                    held: fs.probe == 2,
                    cpu_share: self.cpu.share(),
                });
                self.nlog += 1;
            } else if c.probe0 == 2 && fs.probe == 0 && fs.failed != c.failed0 {
                events.push(Event::ShortRun {
                    kmin: c.kmin,
                    cpu_share: self.cpu.share(),
                });
                self.nlog += 1;
            }
        }
        if s.presents.is_multiple_of(WINDOW_PRESENTS) {
            let seconds = match page {
                Some(p) => {
                    let vs = p.stepped()? + self.fixed_sum;
                    let out = (self.window_ns != 0).then(|| {
                        let game = vs.saturating_sub(self.window_vsyncs) as f64 / 60.0;
                        (game, s.now_ns.saturating_sub(self.window_ns) as f64 / 1e9)
                    });
                    self.window_ns = s.now_ns;
                    self.window_vsyncs = vs;
                    out
                }
                None => None,
            };
            events.push(Event::Window {
                held: self.held,
                seconds,
            });
            self.held = [0; 3];
        }
        Ok(())
    }
}

/// `NXKIT_PACE_DIAG=1` prints the mean frame interval, pacing wait, CPU cost and GPU time (in vsyncs) of every 120
/// presents, to see which of them keeps the hold up.
fn kmax_diag(cfg: &LoadConfig, hz: i32, cap: u32) -> String {
    format!(
        "cap {cap} hz {hz} hold-cfg {} at120 {}",
        cfg.hold,
        cfg.at_120()
    )
}

fn cost_diag(shown: f64, wait: f64, gpu: f64, fs: &FrameStep, cfg: String) {
    let k = fs.k;
    if std::env::var("NXKIT_PACE_DIAG").is_ok_and(|v| v == "1")
        && shown > f64::from(k.max(1)) + 0.25
    {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() % 1_000_000);
        eprintln!(
            "nxkit-pacing: late t {ms} hold {k}: shown {shown:.2} cpu {:.2} gpu {gpu:.2} vsyncs",
            shown - wait
        );
    }
    use std::sync::{Mutex, OnceLock};
    static ON: OnceLock<bool> = OnceLock::new();
    static SUM: Mutex<[f64; 5]> = Mutex::new([0.0; 5]);
    if !*ON.get_or_init(|| std::env::var("NXKIT_PACE_DIAG").is_ok_and(|v| v == "1")) {
        return;
    }
    let Ok(mut a) = SUM.lock() else { return };
    a[0] += shown;
    a[1] += wait;
    a[2] += shown - wait;
    a[3] += gpu;
    a[4] += 1.0;
    if a[4] >= 120.0 {
        let n = a[4];
        eprintln!(
            "nxkit-pacing: diag hold {k}: shown {:.2} wait {:.2} cpu {:.2} gpu {:.2} vsyncs (mean of {n})",
            a[0] / n,
            a[1] / n,
            a[2] / n,
            a[3] / n
        );
        *a = [0.0; 5];
        eprintln!(
            "nxkit-pacing: diag state fit_run {} late_run {} trial {} probe {} hold {:.0} since {:.0} late {} ups {} downs {} probes {} failed {} debt {:.2} {cfg}",
            fs.fit_run,
            fs.late_run,
            fs.trial,
            fs.probe,
            fs.hold,
            fs.since,
            fs.late,
            fs.ups,
            fs.downs,
            fs.probes,
            fs.failed,
            fs.debt
        );
    }
}

/// What a present knew before the step ran, for the report of its changes.
struct Change {
    k0: i32,
    kmin: i32,
    kmin0: i32,
    probe0: i32,
    probes0: u64,
    failed0: u64,
    fixed: i32,
    cost: f64,
    gpu: f64,
    cslow: bool,
    gslow: bool,
}

mod types;

pub use types::*;

#[cfg(test)]
mod tests;
