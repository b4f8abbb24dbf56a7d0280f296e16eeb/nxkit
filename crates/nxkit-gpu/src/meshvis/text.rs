//! The text of `NXKIT_DIAG=meshvis`: the lines of a frame from the counted mesh draws.

use nxkit_nvn::resource::TexId;

use super::timeline::{self, Timed};
use crate::draws::FastMap;

/// The most group lines of a frame.
pub(crate) const MAX_GROUPS: usize = 200;

/// The kind of a mesh draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Mesh,
    MeshIndirect,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::MeshIndirect => "mesh-indirect",
        }
    }
}

/// An attachment of a pass: its texture, format and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Att {
    pub(crate) tex: TexId,
    pub(crate) format: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Att {
    fn text(&self) -> String {
        format!(
            "{:?} {} {}x{}",
            self.tex, self.format, self.width, self.height
        )
    }
}

/// A render pass with mesh draws.
#[derive(Clone, Debug)]
pub(crate) struct PassInfo {
    pub(crate) index: usize,
    pub(crate) colours: Vec<Att>,
    pub(crate) depth: Option<Att>,
}

/// What happened to the count of a mesh draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Counted: the samples it passed.
    Samples(u64),
    /// The game's own report counts the draw, so it was left alone.
    Shared,
    /// The frame had more mesh draws than offsets.
    OverLimit,
    /// The draw got an offset, but it was not encoded (a failed pipeline, a render condition).
    NotCounted,
}

/// The workgroups of a mesh draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Work {
    /// The count of the draw, or of its record in guest memory.
    Groups(u32),
    /// The record of an indirect draw could not be read.
    Unread,
}

/// One mesh draw with its count.
#[derive(Clone, Debug)]
pub(crate) struct DrawRec {
    pub(crate) pass: usize,
    pub(crate) kind: Kind,
    pub(crate) task: String,
    pub(crate) mesh: String,
    pub(crate) fragment: String,
    pub(crate) depth: String,
    pub(crate) write: bool,
    pub(crate) cull: String,
    pub(crate) outcome: Outcome,
    pub(crate) work: Work,
}

type GroupKey = (Kind, String, String, String, String, bool, String);

impl DrawRec {
    fn key(&self) -> GroupKey {
        (
            self.kind,
            self.task.clone(),
            self.mesh.clone(),
            self.fragment.clone(),
            self.depth.clone(),
            self.write,
            self.cull.clone(),
        )
    }
}

/// The draws of one group in one pass.
#[derive(Default)]
struct Group {
    draws: u32,
    nonzero: u32,
    uncounted: u32,
    samples: u64,
    max: u64,
    work: u64,
    zero: u32,
    unread: u32,
}

/// The names of the knobs that are on, or `-`.
pub(crate) fn knob_text(names: &[&str]) -> String {
    if names.is_empty() {
        "-".to_owned()
    } else {
        names.join(",")
    }
}

/// The text of a frame: a line for each pass with a mesh draw and for each group of its draws, the time lines of the
/// depth textures, and the summary. `draws` are in the order of the frame.
pub(crate) fn lines(
    frame: u64,
    passes: &[PassInfo],
    draws: &[DrawRec],
    events: &[Timed],
    knobs: &[&str],
) -> Vec<String> {
    let mut out = Vec::new();
    let mut listed = 0usize;
    let mut left = 0usize;
    for p in passes {
        let mine: Vec<&DrawRec> = draws.iter().filter(|d| d.pass == p.index).collect();
        if mine.is_empty() {
            continue;
        }
        let depth = p.depth.as_ref().map_or_else(|| "-".to_owned(), Att::text);
        let colours: Vec<String> = p.colours.iter().map(Att::text).collect();
        let counted = mine
            .iter()
            .filter(|d| matches!(d.outcome, Outcome::Samples(_)))
            .count();
        out.push(format!(
            "DIAG meshvis frame {frame} pass {} colour [{}] depth {depth} | mesh draws {} counted {counted}",
            p.index,
            colours.join(", "),
            mine.len()
        ));
        let mut order: Vec<GroupKey> = Vec::new();
        let mut groups: FastMap<GroupKey, Group> = FastMap::default();
        for d in &mine {
            let key = d.key();
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            let g = groups.entry(key).or_default();
            g.draws += 1;
            match d.outcome {
                Outcome::Samples(s) => {
                    g.nonzero += u32::from(s > 0);
                    g.samples += s;
                    g.max = g.max.max(s);
                }
                _ => g.uncounted += 1,
            }
            match d.work {
                Work::Groups(w) => {
                    g.work += u64::from(w);
                    g.zero += u32::from(w == 0);
                }
                Work::Unread => g.unread += 1,
            }
        }
        for key in order {
            if listed >= MAX_GROUPS {
                left += 1;
                continue;
            }
            listed += 1;
            let g = &groups[&key];
            let (kind, ts, ms, fs, func, write, cull) = key;
            let uncounted = if g.uncounted > 0 {
                format!(" uncounted {}", g.uncounted)
            } else {
                String::new()
            };
            let unread = if g.unread > 0 {
                format!(" unread {}", g.unread)
            } else {
                String::new()
            };
            out.push(format!(
                "DIAG meshvis frame {frame} pass {} {} ts {ts} ms {ms} fs {fs} depth {func} write {} cull {cull} draws {} nonzero {} samples {} max {}{uncounted} workgroups {} zero {}{unread}",
                p.index,
                kind.name(),
                if write { "on" } else { "off" },
                g.draws,
                g.nonzero,
                g.samples,
                g.max,
                g.work,
                g.zero
            ));
        }
    }
    if left > 0 {
        out.push(format!(
            "DIAG meshvis frame {frame}: {left} more groups not listed"
        ));
    }
    out.extend(timeline::time_lines(frame, events));
    out.push(summary(frame, draws, knobs));
    out
}

/// The summary line of a frame.
fn summary(frame: u64, draws: &[DrawRec], knobs: &[&str]) -> String {
    let (mut counted, mut shared, mut over, mut nonzero, mut samples) = (0, 0, 0, 0, 0u64);
    let (mut work, mut zero, mut unread) = (0u64, 0u32, 0u32);
    for d in draws {
        match d.outcome {
            Outcome::Samples(s) => {
                counted += 1;
                nonzero += u32::from(s > 0);
                samples += s;
            }
            Outcome::Shared => shared += 1,
            Outcome::OverLimit => over += 1,
            Outcome::NotCounted => {}
        }
        match d.work {
            Work::Groups(w) => {
                work += u64::from(w);
                zero += u32::from(w == 0);
            }
            Work::Unread => unread += 1,
        }
    }
    let unread = if unread > 0 {
        format!(", unread {unread}")
    } else {
        String::new()
    };
    format!(
        "DIAG meshvis frame {frame}: mesh draws {} counted {counted} shared {shared} over-limit {over} | draws with samples {nonzero} | samples {samples} | workgroups {work}, draws with no workgroup {zero}{unread} | knobs {}",
        draws.len(),
        knob_text(knobs)
    )
}
