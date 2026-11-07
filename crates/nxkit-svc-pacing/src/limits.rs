//! The frame limits that the host writes the target frame rate into.
//!
//! A game that has no frame step patch follows a frame limiter of its engine. The title module and the family module say
//! where it sits, as data (`nxtitle` `LimitRule`), and the host hands that data over as a [`LimitsConfig`]. This module is
//! the mechanism and knows no game and no engine:
//!
//! - rows at a place of one build, or at an offset that the rewriter found by a name ([`rows`]),
//! - a reflected class that is found by its name ([`reflect`]).
//!
//! Without such a place, and without a frame step state page, nothing makes the game follow a cap. The present path keeps
//! the game's own present interval then ([`Limits::has_engine`]), because presenting at a cap that the game does not step
//! by runs a fixed-step game fast or slow.
//!
//! Source of the mechanism: `lock_find`, `dm_fps_lock_engine` and `dm_fps_lock_tick` in `dm/host/hle_fpslock.c` of nxnative,
//! read on 2026-11-07 at `88c7e03f`. The places are not carried here. A module is `main`. The writes are plain stores that
//! nxnative makes atomic, so they are not guaranteed to be whole for a thread that reads at the same time (unverified).

pub mod reflect;
pub mod rows;

use nxkit_abi::guest::{self, Fault};

use crate::notice::Notice;
use reflect::Reflected;
use rows::{Changed, Kind, Row};

/// An offset that the rewriter found by a name, for example the name of a console variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The name.
    pub key: String,
    /// The offset in the main module.
    pub off: u64,
}

/// What the host knows of the main module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    /// The address of the module's first byte.
    pub base: u64,
    /// The size of the module's image. 0 when it cannot be read.
    pub size: u64,
    /// The build id as 32 hex digits.
    pub build_id: String,
    /// The end of the code. 0 when unknown.
    pub text_end: u64,
    /// The start of the read-only data. 0 when unknown.
    pub ro_off: u64,
    /// The start of the part that the loader added behind the image. 0 for none.
    pub back_off: u64,
    /// The offsets that the rewriter found by a name.
    pub found: Vec<Found>,
}

/// Where a row sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    /// An offset in the main module of one build. `ptr_off` is the offset of a pointer to the object, 0 when the field is
    /// in the module itself.
    Fixed {
        /// The offset of the pointer to the object, or 0.
        ptr_off: u64,
        /// The offset of the field.
        field: u64,
    },
    /// The offset that the rewriter found for `key`, plus `add`. The object pointer is read there and the field is at 0.
    Found {
        /// The name.
        key: &'static str,
        /// Added to the found offset.
        add: u64,
    },
}

/// A row from the title or the family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowSpec {
    /// What it is, for the log.
    pub what: &'static str,
    /// The build id that a [`Site::Fixed`] row is for.
    pub build: Option<&'static str>,
    /// Where it sits.
    pub site: Site,
    /// How it holds the rate.
    pub kind: Kind,
    /// The copies of the value that follow each other.
    pub copies: u32,
}

/// The layout of the reflection records of an engine. All offsets are in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The size of a class record. The first word is the pointer to the class name.
    pub record_size: u64,
    /// The offset in a class record of the pointer to its field list.
    pub fields_at: u64,
    /// The size of one field record. Its first word is the pointer to the field name.
    pub field_stride: u64,
    /// The offset in a field record of the field's offset in the instance.
    pub field_offset_at: u64,
    /// The offset in an instance of the pointer to its type info.
    pub instance_type_at: u64,
}

/// A reflected class with an int field and a float field that hold the frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReflectSpec {
    /// The class name.
    pub class: &'static str,
    /// The name of the int field.
    pub int_field: &'static str,
    /// The name of the float field.
    pub float_field: &'static str,
    /// The layout of the records.
    pub layout: Layout,
}

/// What the host gives the limits: the data of the title and the family, and the facts of the module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitsConfig {
    /// The main module.
    pub module: Module,
    /// The target frame rate of the title: 0 for the game's own limit, 30 or 60.
    pub target: u32,
    /// The rows of the title and the family.
    pub rows: Vec<RowSpec>,
    /// The reflected class of the title or the family.
    pub reflected: Option<ReflectSpec>,
}

/// A line for the log.
#[derive(Clone, Debug, PartialEq)]
pub enum LimitEvent {
    /// A row was written.
    Changed(Changed),
    /// The two fields of the reflected class were written.
    Reflected {
        /// The int field before.
        int: i32,
        /// The float field before.
        float: f32,
    },
}

/// The limits of the main module. The render thread owns it, so a tick takes no lock.
#[derive(Debug)]
pub struct Limits {
    base: u64,
    rows: Vec<Row>,
    reflected: Option<Reflected>,
    target: u32,
    logged: u32,
}

fn rows_of(cfg: &LimitsConfig, notices: &mut Vec<Notice>) -> Vec<Row> {
    let m = &cfg.module;
    let (mut fixed, mut matched) = (0u32, 0u32);
    let mut rows = Vec::new();
    for r in &cfg.rows {
        match r.site {
            Site::Fixed { ptr_off, field } => {
                fixed += 1;
                if r.build.is_some_and(|b| b == m.build_id) {
                    matched += 1;
                    rows.push(Row::new(r.what, ptr_off, field, r.kind, r.copies));
                }
            }
            Site::Found { key, add } => match m.found.iter().find(|f| f.key == key) {
                Some(f) => rows.push(Row::new(r.what, f.off + add, 0, r.kind, r.copies)),
                None => notices.push(Notice::red(format!(
                    "main: {} ({key}) was not found by the rewriter, so the game's own value stays",
                    r.what
                ))),
            },
        }
    }
    if fixed > 0 && matched == 0 {
        notices.push(Notice::red(format!(
            "main: build {} has no frame limit entry, so the game's own frame limit stays",
            m.build_id
        )));
    }
    rows
}

impl Limits {
    /// Finds the limits of the module and checks the target. The notices are for the log. A [`Limits`] with nothing to write
    /// ticks to nothing.
    pub fn new(cfg: &LimitsConfig) -> (Self, Vec<Notice>) {
        let m = &cfg.module;
        let mut notices = Vec::new();
        let rows = rows_of(cfg, &mut notices);
        let reflected = cfg.reflected.as_ref().and_then(|spec| {
            if m.size == 0 {
                notices.push(Notice::red(format!(
                    "main: the image of the module cannot be read, so the class {} is not searched",
                    spec.class
                )));
                return None;
            }
            match guest::with_slice(m.base, m.size, |img| reflect::find(img, m, spec)) {
                Ok(Ok(f)) => Some(f),
                Ok(Err(why)) => {
                    notices.push(Notice::red(why.text(spec)));
                    None
                }
                Err(_) => {
                    notices.push(Notice::red(format!(
                        "main: the image of the module cannot be read, so the class {} is not searched",
                        spec.class
                    )));
                    None
                }
            }
        });
        let mut me = Self {
            base: m.base,
            rows,
            reflected,
            target: 0,
            logged: 0,
        };
        if !matches!(cfg.target, 0 | 30 | 60) {
            notices.push(Notice::red(format!(
                "the target frame rate {} must be 0, 30 or 60 for an engine limit, so the game's own limit stays",
                cfg.target
            )));
        } else if me.has_engine() && cfg.target == 0 {
            notices.push(Notice::info(
                "main: the frame limits stay as the game sets them",
            ));
        } else {
            me.target = cfg.target;
        }
        (me, notices)
    }

    /// Whether the host writes the target into a limit of this module.
    pub fn has_engine(&self) -> bool {
        !self.rows.is_empty() || self.reflected.is_some()
    }

    /// The target after its checks, 0 when the game's limit stays.
    pub fn target(&self) -> u32 {
        self.target
    }

    /// Writes the target into the limits. Runs once a present, after the step of the frame. Nothing is allocated unless a
    /// value changes, and the first changes of each row are the only ones that make an event.
    pub fn tick(&mut self) -> Result<Vec<LimitEvent>, Fault> {
        let mut out = Vec::new();
        if let Some(f) = &self.reflected
            && self.target != 0
            && let Some(w) = f.tick(self.base, i32::try_from(self.target).unwrap_or(60))?
            && self.logged < rows::LOG_LIMIT
        {
            self.logged += 1;
            out.push(LimitEvent::Reflected {
                int: w.int,
                float: w.float,
            });
        }
        for r in &mut self.rows {
            out.extend(
                r.tick(self.base, self.target)?
                    .into_iter()
                    .map(LimitEvent::Changed),
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
