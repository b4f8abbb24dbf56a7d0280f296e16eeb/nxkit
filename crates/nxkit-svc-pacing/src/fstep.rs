//! The frame step in whole vsyncs (R30a).
//!
//! The frame step stubs of a game's rewrite data step the game by the vsyncs that the host gives them each frame. The host
//! keeps each frame on screen for `k` vsyncs. At a steady rate the step is the time on screen, so the motion is even.
//! Real time that the game falls behind (a late frame, a hitch) is a debt, and the host pays it back with one extra vsync
//! on a frame, so over time the game time equals the real time.
//!
//! `k` follows the cost of the frames. Three late frames in a row step it up to the cost. Thirty frames in a row whose
//! cost would fit `k - 1` try the shorter hold. A trial that fails waits twice as long before the next one (1 s up to 8 s),
//! so a device on the edge does not flap.
//!
//! A probe is a trial back to the shortest hold that the cost does not justify. It starts while the caller reports that
//! the CPU runs slow for a reason other than heat, or when the cost at the longer hold is the cost that held the shorter
//! one. The first frames of a probe may be late without failing it, because the clock of the processor needs time to rise
//! for the shorter deadline.
//!
//! The logic is ported from `dm/gfx/nvn/nvn_fstep.h` (`nvn_fstep_next`, `nvn_fstep_gpu_slowed`) at the head of
//! pull request 76, read on 2026-11-07. The thresholds are own and come from measurements on its devices
//! (iPhone and M5 Pro). They are not measured again on nxkit.

/// The longest hold. The interval logic of Breath of the Wild and Tears of the Kingdom uses 1 to 3, so their integer
/// readers never see 4.
pub const FSTEP_MAX: i32 = 3;
/// A frame is late when it or its GPU work took this many vsyncs more than `k`.
pub const FSTEP_LATE: f64 = 0.1;
/// A frame would fit `k - 1` when its cost is at most this share of it.
pub const FSTEP_FIT: f64 = 0.85;
/// Frames in a row that fit `k - 1` before a trial, and frames that a trial must hold.
pub const FSTEP_RUN: i32 = 30;
/// The debt that is kept at most (0.25 s). The rest of a long stall is dropped and not replayed.
pub const FSTEP_DEBT_MAX: f64 = 15.0;
/// Vsyncs since the last step up before a trial (1 s). It doubles after each failed trial.
pub const FSTEP_HOLD0: f64 = 60.0;
/// The longest wait before a trial (8 s).
pub const FSTEP_HOLD_MAX: f64 = 480.0;
/// The longest wait before a trial when only the work judges a frame ([`FrameStep::work`], 2 s): after a few failed
/// trials the frame rate stayed down for seconds although the frames fitted again, and the GPU clock fell further.
pub const FSTEP_HOLD_MAX_WORK: f64 = 120.0;
/// With [`FrameStep::work`]: a frame whose work took this many vsyncs more than `k` is a hitch (a load, a stall), not a
/// sign that the work does not fit. Its time is paid back as debt, and it does not count toward a step up.
pub const FSTEP_HITCH: f64 = 3.0;
/// The first frames of a probe, whose lateness neither fails it nor counts toward a step up.
pub const FSTEP_GRACE: i32 = 30;
/// The cost at the longer hold against the cost that held the shorter one. At most this much is the same scene.
pub const FSTEP_SAME: f64 = 1.1;
/// The fall in cost from one half of a grace window to the next that shows the clock still rising.
pub const FSTEP_RAMP: f64 = 0.05;
/// The grace frames that a probe may take in all.
pub const FSTEP_PROBE_MAX: i32 = 180;
/// The GPU time at the longer hold against its time at the shortest hold that counts as slowed.
pub const FSTEP_GPU_SLOW: f64 = 1.3;
/// The frames at the longer hold before the GPU comparison counts.
pub const FSTEP_GPU_N: i32 = 30;

const SLOTS: usize = FSTEP_MAX as usize + 1;

/// The verdict of the screen on the frame, when the host can tell it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// The frame is judged by its time and cost.
    #[default]
    Unjudged,
    /// The screen showed a recent frame on time.
    OnTime,
    /// The screen showed a recent frame late or dropped it.
    Late,
}

/// The input of one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInput {
    /// Real time since the previous frame's start, in vsyncs.
    pub shown: f64,
    /// The frame's work, the larger of its CPU and GPU time, in vsyncs.
    pub cost: f64,
    /// The shortest hold. It is 1 at a cap of 60 fps and 2 at 30.
    pub kmin: i32,
    /// The longest hold.
    pub kmax: i32,
    /// A step that the game has set itself, in vsyncs, or 0. The host holds it as it is.
    pub fixed: i32,
    /// The CPU runs below its speed for a reason other than heat, so a cost above `k - 1` is no evidence against `kmin`.
    pub slow: bool,
}

/// The state of the frame step.
#[derive(Clone, Debug, Default)]
pub struct FrameStep {
    /// The vsyncs that each frame stays on screen.
    pub k: i32,
    /// The step handed to the game for the next frame. It is `k`, or `k + 1` while the host pays back.
    pub step: i32,
    /// The `kmin` of the last call. A lower one takes effect at once.
    pub kmin: i32,
    /// Real time that the game is behind, in vsyncs.
    pub debt: f64,
    /// Late frames in a row.
    pub late_run: i32,
    /// Frames in a row whose cost fits `k - 1`.
    pub fit_run: i32,
    /// Frames left of a trial at `k - 1`. 0 is no trial.
    pub trial: i32,
    /// 1 while the trial in progress is a probe. 2 after a probe held and its run at `kmin` has not ended.
    pub probe: i32,
    /// Frames left of the probe's grace.
    pub grace: i32,
    /// The grace of a probe is renewed while its frames keep getting cheaper. The caller sets it and it stays.
    pub ramp: bool,
    // The cost of the first and the second half of the current grace window, its late frames and the grace frames that
    // this probe has taken in all.
    pa: f64,
    pb: f64,
    plate: i32,
    ptotal: i32,
    /// Vsyncs that a trial waits after a step up.
    pub hold: f64,
    /// Vsyncs since the last step up.
    pub since: f64,
    /// The ticks that a vsync of 1/60 s is. 0 is 1. At 120 fps the unit is 1/120 s.
    pub per_vsync: i32,
    /// Vsyncs that a frame on time may show longer than `k`. It is one refresh on a screen whose rate is not a multiple of 60.
    pub shown_slack: f64,
    /// The screen's verdict for the next call. The call clears it.
    pub screen: Screen,
    /// Only the work judges a frame (`game.json` key `pace_work`): a frame is late when its cost does not fit the
    /// hold, not when it was shown longer; a hitch does not count; a trial waits at most [`FSTEP_HOLD_MAX_WORK`].
    pub work: bool,
    /// Vsyncs at `kmin` in a row.
    pub run: f64,
    /// The running mean of the cost of the frames on time at `kmin`.
    pub crun: f64,
    /// `crun` when the last run of `FSTEP_HOLD_MAX` or more ended in a step up or a cap change. 0 is none yet.
    pub cref: f64,
    /// The running mean of the cost of the frames on time at the longer hold `ck`.
    pub ccur: f64,
    /// The longer hold that `ccur` belongs to.
    pub ck: i32,
    /// The count of frames in `ccur`.
    pub cn: i32,
    /// The `kmin` that `run`, `crun` and `cref` belong to.
    pub ckmin: i32,
    cref_at: [f64; SLOTS],
    /// Late frames.
    pub late: u64,
    /// Extra vsyncs that paid back a debt.
    pub paid: u64,
    /// Steps up of the hold.
    pub ups: u64,
    /// Steps down of the hold.
    pub downs: u64,
    /// Trials that failed.
    pub failed: u64,
    /// Probes that started.
    pub probes: u64,
    /// Vsyncs of debt that were dropped after a long stall.
    pub dropped: f64,
}

impl FrameStep {
    /// Whether the cost at the longer hold is the cost that held `kmin`. It starts a probe without a slowed report.
    pub fn same_scene(&self) -> bool {
        self.cref > 0.0 && self.cn >= FSTEP_RUN && self.ccur <= FSTEP_SAME * self.cref
    }

    fn start_probe(&mut self, kmin: i32) {
        self.k = kmin;
        self.probes += 1;
        self.trial = FSTEP_RUN;
        self.probe = 1;
        self.grace = FSTEP_GRACE;
        self.fit_run = 0;
        self.late_run = 0;
        self.pa = 0.0;
        self.pb = 0.0;
        self.plate = 0;
        self.ptotal = 0;
    }

    /// Runs once a frame and returns the step for the next frame. `self.k` is how long that frame stays on screen.
    pub fn next(&mut self, input: &FrameInput) -> i32 {
        let pv = f64::from(self.per_vsync.max(1));
        let hold_max = if self.work {
            FSTEP_HOLD_MAX_WORK
        } else {
            FSTEP_HOLD_MAX
        };
        let (hold0, hold_max) = (FSTEP_HOLD0 * pv, hold_max * pv);
        let kmin = input.kmin.max(1);
        let kmax = input.kmax.min(FSTEP_MAX).max(kmin);
        if self.hold == 0.0 {
            self.hold = hold0;
        }
        if input.fixed > 0 {
            // The game's own step. The speed is right by construction and nothing is paid back.
            self.k = input.fixed.min(FSTEP_MAX);
            self.step = self.k;
            self.debt = 0.0;
            self.late_run = 0;
            self.fit_run = 0;
            self.trial = 0;
            self.probe = 0;
            self.grace = 0;
            self.run = 0.0;
            self.kmin = FSTEP_MAX + 1;
            return self.step;
        }
        if self.k == 0 {
            self.k = kmin;
            self.step = kmin;
            self.kmin = kmin;
            return self.step;
        }
        let held = self.k;
        self.debt += input.shown - f64::from(self.step);
        if self.debt > FSTEP_DEBT_MAX {
            self.dropped += self.debt - FSTEP_DEBT_MAX;
            self.debt = FSTEP_DEBT_MAX;
        }
        if self.debt < -1.0 {
            self.debt = -1.0;
        }
        self.since += input.shown;
        let cap_probe = kmin < self.kmin && self.kmin <= FSTEP_MAX && self.k > kmin;
        if cap_probe {
            self.start_probe(kmin);
        } else if kmin < self.kmin || self.k < kmin {
            self.k = kmin;
        }
        if self.k > kmax {
            self.k = kmax;
        }
        self.kmin = kmin;
        let screen = std::mem::take(&mut self.screen);
        let hk = f64::from(held);
        // With `work`, a frame is late when its work did not fit the hold: the game's time without the pacing wait, or
        // the GPU time (`cost`). How long it was shown also holds what the game waits for by itself and the jitter of an
        // adaptive screen, and a longer hold makes neither shorter (Ocarina of Time stepped from 30 to 20 fps at a cost
        // of 0.71 vsyncs on a ProMotion screen). A hitch is paid back and not counted.
        let late = if self.work {
            let hitch = input.cost > hk + FSTEP_HITCH;
            !hitch
                && match screen {
                    Screen::Unjudged => input.cost > hk + FSTEP_LATE,
                    Screen::Late => true,
                    Screen::OnTime => false,
                }
        } else {
            match screen {
                Screen::Unjudged => {
                    input.shown > hk + FSTEP_LATE + self.shown_slack || input.cost > hk + FSTEP_LATE
                }
                Screen::Late => true,
                Screen::OnTime => false,
            }
        };
        let grace = self.grace > 0;
        if grace && !cap_probe {
            // The completed frame at the old cap does not use up the new hold's grace.
            if self.grace > FSTEP_GRACE / 2 {
                self.pa += input.cost;
            } else {
                self.pb += input.cost;
            }
            self.plate += i32::from(late);
            self.ptotal += 1;
            self.grace -= 1;
            if self.grace == 0
                && self.ramp
                && self.plate > 0
                && self.ptotal + FSTEP_GRACE <= FSTEP_PROBE_MAX
                && self.pb <= (1.0 - FSTEP_RAMP) * self.pa
            {
                self.grace = FSTEP_GRACE;
                self.pa = 0.0;
                self.pb = 0.0;
                self.plate = 0;
            }
        }
        if late {
            self.late += 1;
            self.late_run = if grace { 0 } else { self.late_run + 1 };
            self.fit_run = 0;
        } else {
            self.late_run = 0;
        }
        if kmin != self.ckmin {
            // Another cap. Its run starts over and what held it before comes back.
            if (1..=FSTEP_MAX).contains(&self.ckmin) {
                if self.run >= hold_max && self.crun > 0.0 {
                    self.cref = self.crun;
                }
                self.cref_at[self.ckmin as usize] = self.cref;
            }
            self.ckmin = kmin;
            self.run = 0.0;
            self.crun = 0.0;
            self.cref = if kmin <= FSTEP_MAX {
                self.cref_at[kmin as usize]
            } else {
                0.0
            };
        }
        // A frame that the screen showed on time while its cost did not fit is not what the hold costs.
        let fits = screen == Screen::Unjudged || input.cost <= hk;
        if held <= kmin {
            self.run += input.shown;
            self.cn = 0;
            if !late && fits && self.probe != 1 {
                self.crun = if self.crun > 0.0 {
                    self.crun + (input.cost - self.crun) / 32.0
                } else {
                    input.cost
                };
            }
        } else {
            self.run = 0.0;
            if held != self.ck {
                self.ck = held;
                self.cn = 0;
            }
            if !late && fits {
                self.ccur = if self.cn > 0 {
                    self.ccur + (input.cost - self.ccur) / 8.0
                } else {
                    input.cost
                };
                self.cn += 1;
            }
        }
        if self.late_run >= 3 && self.k < kmax {
            // Step up to the cost, at least one vsync.
            let want = ((input.cost - 0.05).ceil() as i32)
                .max(self.k + 1)
                .min(kmax);
            if self.trial > 0 || (self.probe == 2 && self.since < hold_max) {
                self.failed += 1;
                self.trial = 0;
                self.hold = (self.hold * 2.0).min(hold_max);
            } else if self.probe == 2 {
                self.hold = hold0;
            }
            if held == kmin && self.run >= hold_max {
                self.cref = self.crun;
            }
            self.k = want;
            self.ups += 1;
            self.late_run = 0;
            self.fit_run = 0;
            self.since = 0.0;
            self.probe = 0;
        } else if self.trial > 0 && !grace {
            self.trial -= 1;
            if self.trial == 0 {
                if self.probe > 0 {
                    self.probe = 2;
                    self.since = 0.0;
                } else {
                    self.hold = hold0;
                }
            }
        }
        if !late {
            self.fit_run = if self.k > kmin && input.cost <= FSTEP_FIT * f64::from(self.k - 1) {
                self.fit_run + 1
            } else {
                0
            };
        }
        if self.fit_run >= FSTEP_RUN && self.since >= self.hold && self.trial == 0 {
            self.k -= 1;
            self.downs += 1;
            self.trial = FSTEP_RUN;
            self.fit_run = 0;
        } else if (input.slow || self.same_scene())
            && self.k > kmin
            && self.since >= self.hold
            && self.trial == 0
        {
            // A probe goes straight to `kmin`, so the controller of the processor sees the whole deadline.
            self.downs += 1;
            self.start_probe(kmin);
        }
        self.step = self.k + i32::from(self.debt >= 1.0 && self.k < FSTEP_MAX);
        self.paid += u64::from(self.step > self.k);
        self.step
    }
}

/// The GPU time at the shortest hold and at the current hold. The GPU clock follows the hold: on a phone the GPU time of
/// the same work grew from 14.4 to about 25 ms at hold 2 in the engine's measurements, and the cost never fitted hold 1 again.
#[derive(Clone, Debug, Default)]
pub struct GpuSlow {
    /// The GPU time at `kmin`, a running mean.
    pub reference: f64,
    /// The GPU time at the current hold, a running mean.
    pub current: f64,
    n: i32,
    k: i32,
    ref_at: [f64; SLOTS],
}

impl GpuSlow {
    fn slow(&self) -> bool {
        self.n >= FSTEP_GPU_N
            && self.reference > 0.0
            && self.current >= FSTEP_GPU_SLOW * self.reference
            && self.current > FSTEP_FIT * f64::from(self.k - 1)
    }

    /// The ratio of the GPU time at the longer hold to its time at `kmin`, for the log.
    pub fn ratio(&self) -> f64 {
        if self.reference > 0.0 {
            self.current / self.reference
        } else {
            0.0
        }
    }

    /// Runs once a frame with the frame's GPU time in vsyncs (0 is none finished yet), the hold `k` it ran under, `kmin`
    /// and the probe state of the frame step. Returns whether the GPU is slowed.
    pub fn slowed(&mut self, gpu: f64, k: i32, kmin: i32, probe: i32) -> bool {
        if !(1..=FSTEP_MAX).contains(&kmin) {
            return false;
        }
        let slot = kmin as usize;
        self.reference = self.ref_at[slot];
        if gpu <= 0.0 {
            return self.slow();
        }
        if k <= kmin {
            if probe != 1 {
                self.reference = if self.reference > 0.0 {
                    self.reference + (gpu - self.reference) / 32.0
                } else {
                    gpu
                };
                self.ref_at[slot] = self.reference;
            }
            self.n = 0;
            self.k = k;
            return false;
        }
        if k != self.k {
            self.k = k;
            self.n = 0;
        }
        self.current = if self.n > 0 {
            self.current + (gpu - self.current) / 8.0
        } else {
            gpu
        };
        self.n += 1;
        self.slow()
    }
}

#[cfg(test)]
mod tests;
