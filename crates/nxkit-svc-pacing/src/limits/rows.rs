//! The rows of frame limits that the host writes: one value in the game's memory each.
//!
//! A row names a value of a game's frame limiter, and how it holds the target frame rate. The title module gives the rows
//! as data. The host writes the wanted value each frame that the current one differs, because the game may set it again.
//! A value that is not in the range of a frame rate is left alone. It is not set yet.
//!
//! Source: the tick loop of `dm/host/hle_fpslock.c` of nxnative, read on 2026-11-07 at `88c7e03f`. The value kinds and the
//! ranges are nxnative's. The rows are not, they are in the title modules.

use nxkit_abi::guest::{self, Fault};

/// The lowest address that a guest read is tried at.
const LOW: u64 = 0x1000;
/// The changes of a row that become events. Later ones are written but not reported.
pub const LOG_LIMIT: u32 = 8;

/// How a value holds the frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A float of the frame rate.
    FpsF32,
    /// An int of the present interval in vsyncs, 1 to 4.
    VsyncsI32,
    /// A float of the milliseconds of a frame, 1000 / fps. At 60 the margin of half a millisecond keeps a frame limiter that
    /// sleeps up to the time from making a frame that comes a refresh late.
    MsF32,
    /// An int of the frame rate.
    FpsI32,
    /// An unsigned int of system ticks for a frame, `scale / fps`.
    TicksU32 {
        /// The ticks of a second.
        scale: u32,
    },
    /// A float of the seconds of a frame.
    SecsF32,
    /// A switch that an engine reads before it takes the other fields at all. Written 1 at any target when it is 0.
    ModeU32,
    /// A double of the frame rate. `copies` of them follow each other.
    FpsF64,
    /// An unsigned int of the present interval in vsyncs minus one, 0 or 1.
    IntervalMinusOneU32,
    /// A word that the game presets and the host replaces, at any target. Written `to` when it holds `from`.
    PinU32 {
        /// The value that the game presets.
        from: u32,
        /// The value that the host writes.
        to: u32,
    },
}

impl Kind {
    /// Whether the value depends on the target. A pinned word does not, so it is written while the game's limit stays.
    pub fn needs_target(self) -> bool {
        !matches!(self, Self::PinU32 { .. })
    }
}

/// A frame limit in the memory of the game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// What it is, for the log.
    pub what: &'static str,
    /// The offset in the main module of a pointer to the object. 0 when the field sits in the module itself.
    pub ptr_off: u64,
    /// The offset of the field in the object or the module.
    pub field: u64,
    /// How it holds the rate.
    pub kind: Kind,
    /// The copies of the value that follow each other.
    pub copies: u32,
    pub(super) logged: u32,
    pub(super) warned: bool,
}

impl Row {
    /// A row.
    pub fn new(what: &'static str, ptr_off: u64, field: u64, kind: Kind, copies: u32) -> Self {
        Self {
            what,
            ptr_off,
            field,
            kind,
            copies,
            logged: 0,
            warned: false,
        }
    }
}

/// What a row changed, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    /// What it is. Rows with copies say which copy.
    pub what: String,
    /// The value before.
    pub was: String,
    /// The value now.
    pub now: String,
}

/// The milliseconds of a frame at `fps` (`ms_want`).
pub fn ms_want(fps: u32) -> f32 {
    1000.0 / fps as f32 - if fps == 60 { 0.5 } else { 0.0 }
}

/// A new 4-byte value for `cur`, with the text of the change. `None` when the value is already the wanted one or is not
/// set yet.
pub fn change32(kind: Kind, target: u32, cur: u32) -> Option<(u32, String, String)> {
    let f = f32::from_bits(cur);
    match kind {
        Kind::FpsF32 | Kind::MsF32 => {
            let ms = kind == Kind::MsF32;
            let w = if ms { ms_want(target) } else { target as f32 };
            let ok = if ms {
                (8.0..=100.0).contains(&f)
            } else {
                (10.0..=120.0).contains(&f)
            };
            if cur == w.to_bits() || !ok {
                return None;
            }
            let was = if ms {
                format!("{f:.3} ms")
            } else {
                f.to_string()
            };
            let now = if ms {
                format!("{:.3} ms ({target} fps)", ms_want(target))
            } else {
                format!("{target} fps")
            };
            Some((w.to_bits(), was, now))
        }
        Kind::SecsF32 => {
            let w = 1.0 / target as f32;
            if cur == w.to_bits() || !(1.0 / 120.0..=0.1).contains(&f) {
                return None;
            }
            Some((
                w.to_bits(),
                format!("{f:.5} s"),
                format!("{:.5} s ({target} fps)", 1.0 / f64::from(target)),
            ))
        }
        Kind::TicksU32 { scale } => {
            let want = scale / target;
            if cur == want || cur < scale / 120 || cur > scale / 10 {
                return None;
            }
            Some((want, cur.to_string(), format!("{want} ({target} fps)")))
        }
        Kind::ModeU32 => (cur == 0).then(|| (1, cur.to_string(), format!("1 ({target} fps)"))),
        Kind::FpsI32 | Kind::VsyncsI32 => {
            let fps = kind == Kind::FpsI32;
            // Above 60 the display link paces the frames, so a count of vsyncs stays 1.
            let want = if fps {
                target
            } else if target > 60 {
                1
            } else {
                60 / target
            };
            let (lo, hi) = if fps { (10, 120) } else { (1, 4) };
            if cur == want || cur < lo || cur > hi {
                return None;
            }
            let unit = if fps { "fps" } else { "vsyncs" };
            Some((want, cur.to_string(), format!("{want} {unit}")))
        }
        Kind::IntervalMinusOneU32 => {
            let want = if target > 60 { 0 } else { 60 / target - 1 };
            if cur == want || cur > 1 {
                return None;
            }
            Some((want, cur.to_string(), format!("{want} ({target} fps)")))
        }
        Kind::PinU32 { from, to } => (cur == from && cur != to)
            .then(|| (to, format!("{cur:#x}"), format!("{to:#x} (pinned)"))),
        Kind::FpsF64 => None,
    }
}

impl Row {
    /// The name of copy `c` for the log.
    fn label(&self, c: u64) -> String {
        if self.copies <= 1 {
            return self.what.to_string();
        }
        let which = match (self.kind, c) {
            (Kind::FpsF64, 0) => " (target)",
            (Kind::FpsF64, _) => " (reported rate)",
            (_, 0) => " (game thread)",
            _ => " (render thread)",
        };
        format!("{}{which}", self.what)
    }

    /// Writes the target where the value differs and is set. Nothing is allocated unless a value changes and is logged.
    pub fn tick(&mut self, base: u64, target: u32) -> Result<Vec<Changed>, Fault> {
        let mut out = Vec::new();
        if target == 0 && self.kind.needs_target() {
            return Ok(out);
        }
        let obj = if self.ptr_off != 0 {
            match guest::read_u64(base + self.ptr_off) {
                Ok(o) if o != 0 => o,
                Ok(_) => return Ok(out),
                Err(_) => {
                    // A pointer that cannot be read is told once. 0 only means that the game has not made the object yet.
                    if !self.warned {
                        self.warned = true;
                        out.push(Changed {
                            what: self.what.to_string(),
                            was: "unreadable".to_string(),
                            now: "left as it is".to_string(),
                        });
                    }
                    return Ok(out);
                }
            }
        } else {
            base
        };
        let size: u64 = if self.kind == Kind::FpsF64 { 8 } else { 4 };
        for c in 0..u64::from(self.copies) {
            let a = obj + self.field + size * c;
            if a < LOW {
                break;
            }
            let change = if self.kind == Kind::FpsF64 {
                let Ok(cur) = guest::read_u64(a).map(f64::from_bits) else {
                    break;
                };
                let want = f64::from(target);
                if cur == want || !(10.0..=120.0).contains(&cur) {
                    continue;
                }
                guest::write_u64(a, want.to_bits())?;
                (cur.to_string(), format!("{target} fps"))
            } else {
                let Ok(cur) = guest::read_u32(a) else {
                    break;
                };
                let Some((want, was, now)) = change32(self.kind, target, cur) else {
                    continue;
                };
                guest::write_u32(a, want)?;
                (was, now)
            };
            if self.logged < LOG_LIMIT {
                self.logged += 1;
                out.push(Changed {
                    what: self.label(c),
                    was: change.0,
                    now: change.1,
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(f: f32) -> u32 {
        f.to_bits()
    }

    #[test]
    fn a_frame_rate_float_changes_only_inside_the_range_of_frame_rates() {
        assert_eq!(
            change32(Kind::FpsF32, 60, bits(30.0)).map(|c| c.0),
            Some(bits(60.0))
        );
        assert_eq!(
            change32(Kind::FpsF32, 60, bits(6000.0)),
            None,
            "not set yet"
        );
        assert_eq!(change32(Kind::FpsF32, 60, bits(0.0)), None, "no limit yet");
        assert_eq!(change32(Kind::FpsF32, 60, bits(60.0)), None, "already set");
    }

    #[test]
    fn the_milliseconds_at_60_keep_a_margin() {
        assert_eq!(ms_want(60), 1000.0 / 60.0 - 0.5);
        assert_eq!(ms_want(30), 1000.0 / 30.0);
        assert_eq!(
            change32(Kind::MsF32, 60, bits(33.333)).map(|c| c.0),
            Some(bits(ms_want(60)))
        );
        assert_eq!(change32(Kind::MsF32, 60, bits(2.0)), None);
    }

    #[test]
    fn seconds_ticks_and_mode_follow_the_target() {
        assert_eq!(
            change32(Kind::SecsF32, 60, bits(1.0 / 30.0)).map(|c| c.0),
            Some(bits(1.0 / 60.0))
        );
        assert_eq!(change32(Kind::SecsF32, 60, bits(0.0)), None);
        let t = Kind::TicksU32 { scale: 19_200_000 };
        assert_eq!(change32(t, 60, 640_000).map(|c| c.0), Some(320_000));
        assert_eq!(change32(t, 60, 5), None);
        assert_eq!(change32(Kind::ModeU32, 30, 0).map(|c| c.0), Some(1));
        assert_eq!(change32(Kind::ModeU32, 30, 1), None);
    }

    #[test]
    fn ints_follow_the_target_and_vsyncs_stay_one_above_60() {
        assert_eq!(change32(Kind::FpsI32, 60, 30).map(|c| c.0), Some(60));
        assert_eq!(change32(Kind::FpsI32, 60, 0), None);
        assert_eq!(change32(Kind::VsyncsI32, 30, 1).map(|c| c.0), Some(2));
        assert_eq!(change32(Kind::VsyncsI32, 120, 2).map(|c| c.0), Some(1));
        assert_eq!(change32(Kind::VsyncsI32, 60, 9), None);
    }

    #[test]
    fn a_row_writes_each_copy_through_its_object_pointer() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut mem = vec![0u64; 0x40];
        let base = mem.as_mut_ptr() as u64;
        guest::set_range(base, (mem.len() * 8) as u64);
        assert!(guest::write_u64(base + 0x40, base + 0x100).is_ok());
        assert!(guest::write_u32(base + 0x108, 30).is_ok());
        assert!(guest::write_u32(base + 0x10c, 30).is_ok());
        let mut row = Row::new("two ints", 0x40, 8, Kind::FpsI32, 2);
        let first = row.tick(base, 60);
        assert!(first.as_ref().is_ok_and(|c| c.len() == 2));
        assert_eq!(guest::read_u32(base + 0x108).ok(), Some(60));
        assert_eq!(guest::read_u32(base + 0x10c).ok(), Some(60));
        assert!(
            row.tick(base, 60).is_ok_and(|c| c.is_empty()),
            "nothing left to change"
        );
    }

    #[test]
    fn a_row_waits_for_an_object_that_is_not_made() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut mem = vec![0u64; 0x40];
        let base = mem.as_mut_ptr() as u64;
        guest::set_range(base, (mem.len() * 8) as u64);
        let mut row = Row::new("late", 0x40, 8, Kind::FpsF64, 2);
        assert!(row.tick(base, 60).is_ok_and(|c| c.is_empty()));
        let mut gone = Row::new("gone", 0x4000_0000, 8, Kind::FpsF64, 1);
        assert!(gone.tick(base, 60).is_ok_and(|c| c.len() == 1), "told once");
        assert!(gone.tick(base, 60).is_ok_and(|c| c.is_empty()));
    }

    #[test]
    fn doubles_are_written_as_doubles() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut mem = vec![0u64; 0x40];
        let base = mem.as_mut_ptr() as u64;
        guest::set_range(base, (mem.len() * 8) as u64);
        assert!(guest::write_u64(base + 0x80, 30f64.to_bits()).is_ok());
        assert!(guest::write_u64(base + 0x88, 0f64.to_bits()).is_ok());
        let mut row = Row::new("regulator", 0, 0x80, Kind::FpsF64, 2);
        assert!(row.tick(base, 60).is_ok());
        assert_eq!(guest::read_u64(base + 0x80).ok(), Some(60f64.to_bits()));
        assert_eq!(
            guest::read_u64(base + 0x88).ok(),
            Some(0f64.to_bits()),
            "not set yet"
        );
    }

    #[test]
    fn an_interval_minus_one_and_a_pinned_word_follow_their_rules() {
        assert_eq!(
            change32(Kind::IntervalMinusOneU32, 30, 0).map(|c| c.0),
            Some(1)
        );
        assert_eq!(
            change32(Kind::IntervalMinusOneU32, 60, 1).map(|c| c.0),
            Some(0)
        );
        assert_eq!(change32(Kind::IntervalMinusOneU32, 60, 0), None);
        assert_eq!(change32(Kind::IntervalMinusOneU32, 60, 7), None);
        let pin = Kind::PinU32 { from: 7, to: 9 };
        assert_eq!(change32(pin, 0, 7).map(|c| c.0), Some(9));
        assert_eq!(change32(pin, 0, 8), None);
        assert_eq!(change32(pin, 0, 9), None);
        assert!(!pin.needs_target() && Kind::FpsI32.needs_target());
    }
}
