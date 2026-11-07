//! The engine limiters that the host writes the target frame rate into.
//!
//! A game that has no frame step in whole vsyncs follows a frame limiter of its engine. Where nxnative knows the place of
//! that limiter the host writes the target into it each frame. The places are:
//!
//! - rows for a build ([`builds`]),
//! - the target frame rate that the rewriter found by its readers (`Module::fps_target`),
//! - the Unreal variables that the rewriter found (`Module::unreal`),
//! - NieR:Automata's frame interval ([`nier`]),
//! - Frostbite's simulation rate, found by name ([`frostbite`]).
//!
//! Without such a place, and without a frame step state page, nothing makes the game follow a cap. The present path keeps
//! the game's own present interval then ([`Limits::has_engine`]), because presenting at a cap that the game does not step
//! by runs a fixed-step game fast or slow (Tears of the Kingdom 1.4.2 ran at 2x with a target of 60).
//!
//! Source: `lock_find`, `dm_fps_lock_engine` and `dm_fps_lock_tick` in `dm/host/hle_fpslock.c` of nxnative, read on
//! 2026-11-07 at `88c7e03f`. A module is `main`. The writes are plain stores that nxnative makes atomic, so they are not
//! guaranteed to be whole for a thread that reads at the same time (unverified).

pub mod builds;
pub mod frostbite;
pub mod nier;
pub mod rows;

use nxkit_abi::guest::{self, Fault};

use crate::interval;
use crate::notice::Notice;
use frostbite::Frostbite;
use nier::Nier;
use rows::{Changed, Kind, Row};

/// A variable of Unreal Engine that the rewriter found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnrealCvar {
    /// The name of the variable, for example `t.MaxFPS`.
    pub name: String,
    /// The offset of its `TAutoConsoleVariable` in the main module.
    pub off: u64,
}

/// The target frame rate that the rewriter found: an int32 in the object that a global points to, which the readers of the
/// game divide 60.0 by to step it. The game sets it to 30 or 60 itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FpsTargetField {
    /// The offset of the pointer to the object, 0 when the field is in the module.
    pub ptr_off: u64,
    /// The offset of the field.
    pub field: u64,
}

/// What the host knows of the main module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    /// The address of the module's first byte.
    pub base: u64,
    /// The size of the module's image.
    pub size: u64,
    /// The build id as 32 hex digits.
    pub build_id: String,
    /// The end of the code. 0 when unknown.
    pub text_end: u64,
    /// The start of the read-only data. 0 when unknown.
    pub ro_off: u64,
    /// The start of the part that the loader added behind the image. 0 for none.
    pub back_off: u64,
    /// The target frame rate that the rewriter found.
    pub fps_target: Option<FpsTargetField>,
    /// The Unreal variables that the rewriter found.
    pub unreal: Vec<UnrealCvar>,
}

/// What the profile of a game says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitsConfig {
    /// The main module.
    pub module: Module,
    /// The profile's `fps_target`.
    pub target: u32,
    /// The profile's `fps_max`.
    pub fps_max: u32,
    /// The profile's `ue_fps_cvar`, the frame limit of Unreal.
    pub ue_fps_cvar: String,
    /// Whether NieR's dynamic resolution is on.
    pub dynamic_resolution: bool,
}

/// A line for the log.
#[derive(Clone, Debug, PartialEq)]
pub enum LimitEvent {
    /// A row was written.
    Changed(Changed),
    /// NieR's present interval changed.
    NierInterval {
        /// The interval before.
        from: u32,
        /// The interval now.
        to: u32,
    },
    /// Frostbite's simulation rate changed.
    Frostbite {
        /// `ForceSimRate` before.
        rate: i32,
        /// `MaxSimFps` before.
        max: f32,
    },
}

/// The limiters of the main module.
#[derive(Debug)]
pub struct Limits {
    base: u64,
    rows: Vec<Row>,
    nier: Option<Nier>,
    frostbite: Option<Frostbite>,
    target: u32,
    high: bool,
    fb_logged: u32,
}

/// The names of the Unreal variables that the host writes besides the frame limit of the profile.
const UE_SYNC: [&str; 2] = ["rhi.SyncInterval", "nvn.SyncInterval"];
/// The Unreal rows of a main (the frame limit and the two present intervals).
const UE_ROWS: usize = 3;

fn unreal_rows(m: &Module, fps_cvar: &str) -> (Vec<Row>, bool) {
    let mut rows = Vec::new();
    let mut have = false;
    for c in &m.unreal {
        if rows.len() >= UE_ROWS {
            break;
        }
        let fps = c.name == fps_cvar;
        if !fps && !UE_SYNC.contains(&c.name.as_str()) {
            continue;
        }
        have |= fps;
        let kind = if fps { Kind::FpsF32 } else { Kind::VsyncsI32 };
        rows.push(Row::new(
            format!("Unreal {}", c.name),
            c.off + 0x10,
            0,
            kind,
            2,
        ));
    }
    (rows, !m.unreal.is_empty() && !have)
}

impl Limits {
    /// Finds the limiters of the module and resolves the target. The notices are for the log. A [`Limits`] with nothing to
    /// write ticks to nothing.
    pub fn new(cfg: &LimitsConfig) -> (Self, Vec<Notice>) {
        let m = &cfg.module;
        let mut notices = Vec::new();
        let mut rows = builds::rows_for(&m.build_id);
        if let Some(t) = m.fps_target {
            rows.push(Row::new(
                "target frame rate",
                t.ptr_off,
                t.field,
                Kind::FpsI32,
                1,
            ));
        }
        let (ue, ue_missing) = unreal_rows(m, &cfg.ue_fps_cvar);
        rows.extend(ue);
        let mut nier = None;
        if let Some((n, mut ns)) = Nier::new(&m.build_id, m.base, cfg.dynamic_resolution) {
            notices.append(&mut ns);
            nier = Some(n);
        }
        let frostbite = if m.size == 0 {
            None
        } else {
            match guest::with_slice(m.base, m.size, |img| frostbite::find(img, m)) {
                Ok(Ok(f)) => Some(f),
                Ok(Err(why)) => {
                    notices.extend(why.text().map(Notice::red));
                    None
                }
                Err(_) => {
                    notices.push(Notice::red(
                        "main: the image of the module cannot be read, so Frostbite's simulation rate is not searched",
                    ));
                    None
                }
            }
        };
        let mut me = Self {
            base: m.base,
            rows,
            nier,
            frostbite,
            target: 0,
            high: false,
            fb_logged: 0,
        };
        if !me.has_engine() && !ue_missing {
            return (me, notices);
        }
        let resolved = interval::resolve_target(cfg.target, cfg.fps_max);
        notices.extend(resolved.notice);
        let t = resolved.fps;
        if t != 0 && ue_missing {
            notices.push(Notice::red(format!(
                "main: Unreal {} not found by the rewriter, so the game's own frame limit stays",
                cfg.ue_fps_cvar
            )));
        }
        if !me.has_engine() {
            return (me, notices);
        }
        if t == 0 {
            for r in &me.rows {
                notices.push(Notice::info(format!(
                    "main: {} left as the game sets it",
                    r.what
                )));
            }
            if me.nier.is_some() {
                notices.push(Notice::info(
                    "main: NieR frame interval left as the game sets it",
                ));
            }
            if me.frostbite.is_some() {
                notices.push(Notice::info(
                    "main: Frostbite simulation rate left as the game sets it",
                ));
            }
            return (me, notices);
        }
        me.target = t;
        me.high = t == interval::HIGH_TARGET;
        (me, notices)
    }

    /// Whether the host writes the target into an engine limiter of this module.
    pub fn has_engine(&self) -> bool {
        !self.rows.is_empty() || self.nier.is_some() || self.frostbite.is_some()
    }

    /// The target after its checks, 0 when the game's limit stays.
    pub fn target(&self) -> u32 {
        self.target
    }

    /// Whether the target is 120, which follows [`interval::high_target_now`] frame by frame.
    pub fn runs_at_120(&self) -> bool {
        self.high
    }

    /// Writes the target into the limiters. `hold` is the hold of the frame step, which decides between 120 and 60 at a
    /// target of 120. Runs once a present, after the step of the frame.
    pub fn tick(&mut self, hold: u32) -> Result<Vec<LimitEvent>, Fault> {
        let mut out = Vec::new();
        if let Some(n) = &self.nier {
            n.resolution_tick(self.base)?;
        }
        if self.target == 0 {
            return Ok(out);
        }
        let target = if self.high {
            interval::high_target_now(hold)
        } else {
            self.target
        };
        if let Some(n) = &mut self.nier
            && let Some((from, to)) = n.interval_tick(self.base, target)?
        {
            out.push(LimitEvent::NierInterval { from, to });
        }
        if let Some(f) = &self.frostbite
            && let Some(w) = f.tick(self.base, i32::try_from(target).unwrap_or(60))?
            && self.fb_logged < rows::LOG_LIMIT
        {
            self.fb_logged += 1;
            out.push(LimitEvent::Frostbite {
                rate: w.rate,
                max: w.max,
            });
        }
        for r in &mut self.rows {
            out.extend(
                r.tick(self.base, target)?
                    .into_iter()
                    .map(LimitEvent::Changed),
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
