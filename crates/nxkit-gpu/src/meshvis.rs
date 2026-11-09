//! The `meshvis` diagnostic: how many samples each task and mesh draw of a frame passes.
//!
//! A frame can encode all its mesh draws and still show none of their geometry. The counts tell the cases apart:
//! no samples (nothing rasterizes, or all of it is clipped or culled), samples that the depth test would reject (read
//! the order of the passes on the depth texture), or samples in the wrong shapes.
//!
//! On a sampled frame [`plan`] gives every mesh draw an offset of its own in the visibility result buffer of
//! [`crate::queries`]. The offsets follow the offsets of the game's own reports. The encoder sets the offset right
//! before the draw (`encode::render_pass`), and the next draw sets its own offset or none, so the game's offset counts
//! again. A draw that the game's own report counts keeps that offset and is marked `shared`. When the GPU finished the
//! frame, [`Plan::report`] reads the counts and [`lines`] makes the text.
//!
//! A frame is sampled when it holds a mesh draw and it is one of the first [`FIRST_FRAMES`] such frames, or its number
//! is a multiple of [`EVERY`], or the knob `meshvisall` is on. With the category off nothing here runs.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{LoadAction, PassKind, PassPlan, Slot};
use nxkit_nvn::resource::TexId;

use crate::detail::FrameDetail;
use crate::draws::{FastMap, FastSet, Ordinal, Prep};
use crate::textures::Res;

/// The first frames with a mesh draw that are sampled.
pub(crate) const FIRST_FRAMES: u64 = 3;

/// After the first frames, every frame with a number that is a multiple of this is sampled.
pub(crate) const EVERY: u64 = 120;

/// The most mesh draws of a frame that get an offset (8 bytes each). The draws after them are not counted.
pub(crate) const MAX_OFFSETS: usize = 8192;

/// The most group lines of a frame.
pub(crate) const MAX_GROUPS: usize = 200;

/// Whether a frame with a mesh draw is sampled. `earlier` is the number of frames with a mesh draw before it.
pub(crate) fn is_sampled(earlier: u64, frame: u64, all: bool) -> bool {
    all || earlier < FIRST_FRAMES || frame.is_multiple_of(EVERY)
}

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

/// A render pass that the report names.
#[derive(Clone, Debug)]
pub(crate) struct PassInfo {
    pub(crate) index: usize,
    pub(crate) colours: Vec<Att>,
    pub(crate) depth: Option<Att>,
    /// The load action of the depth attachment, as text.
    pub(crate) load: String,
    /// The clear depth, when the pass clears it.
    pub(crate) clear: Option<f32>,
    /// All draws of the pass.
    pub(crate) draws: u32,
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
}

/// The text of a frame: a line for each pass with a mesh draw and for each group of its draws, a line for each depth
/// pass on the same texture without one, and the summary. `draws` are in the order of the frame.
pub(crate) fn lines(frame: u64, passes: &[PassInfo], draws: &[DrawRec]) -> Vec<String> {
    let mut out = Vec::new();
    let mut listed = 0usize;
    let mut left = 0usize;
    for p in passes {
        let mine: Vec<&DrawRec> = draws.iter().filter(|d| d.pass == p.index).collect();
        let depth = p.depth.as_ref().map_or_else(|| "-".to_owned(), Att::text);
        if mine.is_empty() {
            let clear = p.clear.map_or_else(|| "-".to_owned(), |c| c.to_string());
            out.push(format!(
                "DIAG meshvis frame {frame} pass {} (no mesh draws) depth {} load {} clear {clear} draws {}",
                p.index,
                p.depth
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), |a| format!("{:?}", a.tex)),
                p.load,
                p.draws
            ));
            continue;
        }
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
            out.push(format!(
                "DIAG meshvis frame {frame} pass {} {} ts {ts} ms {ms} fs {fs} depth {func} write {} cull {cull} draws {} nonzero {} samples {} max {}{uncounted}",
                p.index,
                kind.name(),
                if write { "on" } else { "off" },
                g.draws,
                g.nonzero,
                g.samples,
                g.max
            ));
        }
    }
    if left > 0 {
        out.push(format!(
            "DIAG meshvis frame {frame}: {left} more groups not listed"
        ));
    }
    out.push(summary(frame, draws));
    out
}

/// The summary line of a frame.
fn summary(frame: u64, draws: &[DrawRec]) -> String {
    let (mut counted, mut shared, mut over, mut nonzero, mut samples) = (0, 0, 0, 0, 0u64);
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
    }
    format!(
        "DIAG meshvis frame {frame}: mesh draws {} counted {counted} shared {shared} over-limit {over} | draws with samples {nonzero} | samples {samples}",
        draws.len()
    )
}

/// A mesh draw of the frame.
struct Entry {
    pass: usize,
    ord: u32,
    kind: Kind,
    task: String,
    mesh: String,
    fragment: String,
}

/// What the encoder saw of a mesh draw.
#[derive(Default)]
struct Live {
    /// The depth function, the depth write and the cull mode that the draw really used.
    state: Option<(String, bool, String)>,
    /// A Metal draw of it was encoded with its offset set.
    encoded: bool,
}

/// The mesh draws of one sampled frame and their offsets.
pub(crate) struct Plan {
    frame: u64,
    passes: Vec<PassInfo>,
    entries: Vec<Entry>,
    /// The offsets of the game's own reports come first. This is the index of the first one of the plan.
    base: u32,
    /// The draws with an offset: the first this many.
    limit: usize,
    /// The index of an entry by the ordinal of its draw, for the draws with an offset.
    at: FastMap<u32, usize>,
    /// The render passes with a draw that has an offset.
    with_offsets: FastSet<usize>,
    live: Mutex<Vec<Live>>,
}

/// The plan of a frame, None when the category is off or the frame is not sampled.
pub(crate) fn plan(
    frame_no: u64,
    frame: &Frame,
    passes: &PassPlan,
    detail: &FrameDetail,
    ords: &[Ordinal],
    textures: &FastMap<TexId, Res>,
) -> Option<Plan> {
    if !crate::diag("meshvis") || !detail.draws.iter().any(|d| d.args.is_mesh()) {
        return None;
    }
    static MESH_FRAMES: AtomicU64 = AtomicU64::new(0);
    let earlier = MESH_FRAMES.fetch_add(1, Ordering::Relaxed);
    if !is_sampled(earlier, frame_no, crate::knob("meshvisall")) {
        return None;
    }
    let att = |tex: TexId| {
        let (format, width, height) = textures.get(&tex).map_or_else(
            || ("?".to_owned(), 0, 0),
            |r| (format!("{:?}", r.desc.format), r.desc.width, r.desc.height),
        );
        Att {
            tex,
            format,
            width,
            height,
        }
    };
    let key = |k: Option<&std::sync::Arc<str>>| k.map_or_else(|| "-".to_owned(), |k| k.to_string());
    let mut infos = Vec::new();
    let mut entries = Vec::new();
    let mut with_mesh: BTreeSet<usize> = BTreeSet::new();
    for (pi, p) in passes.passes.iter().enumerate() {
        if p.kind != PassKind::Render {
            continue;
        }
        let mut draws = 0;
        for i in p.cmds.clone() {
            if !matches!(frame.cmds.get(i as usize), Some(Cmd::Draw { .. })) {
                continue;
            }
            draws += 1;
            let Some(ord) = ords.get(i as usize).map(|o| o.draw) else {
                continue;
            };
            let Some(d) = detail.draws.get(ord as usize).filter(|d| d.args.is_mesh()) else {
                continue;
            };
            with_mesh.insert(pi);
            entries.push(Entry {
                pass: pi,
                ord,
                kind: if d.args.is_indirect() {
                    Kind::MeshIndirect
                } else {
                    Kind::Mesh
                },
                task: key(d.program.task.as_ref()),
                mesh: key(d.program.mesh.as_ref()),
                fragment: key(d.program.fragment.as_ref()),
            });
        }
        let mut info = PassInfo {
            index: pi,
            colours: Vec::new(),
            depth: None,
            load: String::new(),
            clear: None,
            draws,
        };
        for a in passes.attachments_of(p) {
            match a.slot {
                Slot::Color(_) => info.colours.push(att(a.tex)),
                Slot::Depth => {
                    info.depth = Some(att(a.tex));
                    info.load = format!("{:?}", a.load);
                    if a.load == LoadAction::Clear {
                        info.clear = depth_clear(frame, detail, ords, p.cmds.clone());
                    }
                }
            }
        }
        infos.push(info);
    }
    let depths: BTreeSet<TexId> = infos
        .iter()
        .filter(|i| with_mesh.contains(&i.index))
        .filter_map(|i| i.depth.as_ref().map(|a| a.tex))
        .collect();
    infos.retain(|i| {
        with_mesh.contains(&i.index)
            || (i.draws > 0 && i.depth.as_ref().is_some_and(|a| depths.contains(&a.tex)))
    });
    Some(Plan {
        frame: frame_no,
        passes: infos,
        live: Mutex::new((0..entries.len()).map(|_| Live::default()).collect()),
        entries,
        base: 0,
        limit: 0,
        at: FastMap::default(),
        with_offsets: FastSet::default(),
    })
}

/// The depth of the first full depth clear in the commands `cmds`.
fn depth_clear(
    frame: &Frame,
    detail: &FrameDetail,
    ords: &[Ordinal],
    cmds: std::ops::Range<u32>,
) -> Option<f32> {
    cmds.into_iter()
        .find(|i| {
            matches!(
                frame.cmds.get(*i as usize),
                Some(Cmd::ClearDepthStencil { full: true })
            )
        })
        .and_then(|i| ords.get(i as usize))
        .and_then(|o| detail.clears.get(o.clear as usize))
        .map(|c| c.depth)
}

impl Plan {
    /// The offsets the plan wants in the visibility buffer.
    pub(crate) fn wanted(&self) -> usize {
        self.entries.len().min(MAX_OFFSETS)
    }

    /// Fixes the first offset index and the room of the buffer after the game's offsets.
    pub(crate) fn bind(&mut self, base: usize, room: usize) {
        self.base = u32::try_from(base).unwrap_or(u32::MAX);
        self.limit = self.wanted().min(room);
        for (i, e) in self.entries.iter().enumerate().take(self.limit) {
            self.at.insert(e.ord, i);
            self.with_offsets.insert(e.pass);
        }
    }

    /// True when a draw of render pass `index` has an offset.
    pub(crate) fn has_pass(&self, index: usize) -> bool {
        self.with_offsets.contains(&index)
    }

    /// The byte offset that mesh draw `ord` counts into, None without one.
    pub(crate) fn offset(&self, ord: u32) -> Option<u64> {
        let i = *self.at.get(&ord)?;
        Some((u64::from(self.base) + i as u64) * 8)
    }

    /// The state a mesh draw really used, after the knobs.
    pub(crate) fn note(&self, ord: u32, p: &Prep) {
        let Some(i) = self.at.get(&ord) else { return };
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(l) = live.get_mut(*i) {
            l.state = Some((
                format!("{:?}", p.depth_stencil_desc.depth_compare),
                p.depth_stencil_desc.depth_write,
                format!("{:?}", p.cull),
            ));
        }
    }

    /// A Metal draw of mesh draw `ord` was encoded with its offset set.
    pub(crate) fn encoded(&self, ord: u32) {
        let Some(i) = self.at.get(&ord) else { return };
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(l) = live.get_mut(*i) {
            l.encoded = true;
        }
    }

    /// The text of the frame. `shared` tells whether the game's report counts a draw (by ordinal), `samples` gives the
    /// count of an offset index of the plan. Runs after the GPU finished the frame.
    pub(crate) fn report(
        &self,
        shared: &dyn Fn(u32) -> bool,
        samples: &dyn Fn(u64) -> Option<u64>,
    ) -> Vec<String> {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        let draws: Vec<DrawRec> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let l = live.get(i);
                let outcome = if shared(e.ord) {
                    Outcome::Shared
                } else if i >= self.limit {
                    Outcome::OverLimit
                } else if l.is_some_and(|l| l.encoded) {
                    samples(u64::from(self.base) + i as u64)
                        .map_or(Outcome::NotCounted, Outcome::Samples)
                } else {
                    Outcome::NotCounted
                };
                let (depth, write, cull) = l
                    .and_then(|l| l.state.clone())
                    .unwrap_or_else(|| ("?".to_owned(), false, "?".to_owned()));
                DrawRec {
                    pass: e.pass,
                    kind: e.kind,
                    task: e.task.clone(),
                    mesh: e.mesh.clone(),
                    fragment: e.fragment.clone(),
                    depth,
                    write,
                    cull,
                    outcome,
                }
            })
            .collect();
        lines(self.frame, &self.passes, &draws)
    }
}

#[cfg(test)]
mod tests;
