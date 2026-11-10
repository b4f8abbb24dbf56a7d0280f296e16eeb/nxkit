//! The `meshvis` diagnostic: how many samples each task and mesh draw of a frame passes.
//!
//! A frame can encode all its mesh draws and still show none of their geometry. The counts tell the cases apart:
//! no workgroups (the culling gave nothing), no samples (nothing rasterizes, or all of it is clipped or culled),
//! samples that the depth test would reject (read the time line of the depth texture), or samples in the wrong shapes.
//!
//! On a sampled frame [`plan`] gives every mesh draw an offset of its own in the visibility result buffer of
//! [`crate::queries`]. The offsets follow the offsets of the game's own reports. The encoder sets the offset right
//! before the draw (`encode::render_pass`), and the next draw sets its own offset or none, so the game's offset counts
//! again. A draw that the game's own report counts keeps that offset and is marked `shared`. When the GPU finished the
//! frame, [`Plan::report`] reads the counts and the workgroups of the indirect draws and [`text::lines`] makes the
//! text. The time line of the depth textures is in [`timeline`].
//!
//! A frame is sampled when it holds a mesh draw and it is one of the first [`FIRST_FRAMES`] such frames, or its number
//! is a multiple of [`EVERY`], or the knob `meshvisall` is on. With the category off nothing here runs. With it on,
//! the front end records the conditions of the commands (`nxkit_nvn::rendercond::gate`) on every frame.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use nxkit_metal::GuestSpace;
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{PassKind, PassPlan, Slot};
use nxkit_nvn::resource::TexId;

use crate::detail::{DrawArgs, FrameDetail};
use crate::draws::{FastMap, FastSet, Ordinal, Prep};
use crate::textures::Res;

#[cfg(test)]
mod tests;
mod text;
mod timeline;

use text::{Att, DrawRec, Kind, Outcome, PassInfo, Work};
use timeline::Timed;

/// The first frames with a mesh draw that are sampled.
pub(crate) const FIRST_FRAMES: u64 = 3;

/// After the first frames, every frame with a number that is a multiple of this is sampled.
pub(crate) const EVERY: u64 = 120;

/// The most mesh draws of a frame that get an offset (8 bytes each). The draws after them are not counted.
pub(crate) const MAX_OFFSETS: usize = 8192;

/// Whether a frame with a mesh draw is sampled. `earlier` is the number of frames with a mesh draw before it.
pub(crate) fn is_sampled(earlier: u64, frame: u64, all: bool) -> bool {
    all || earlier < FIRST_FRAMES || frame.is_multiple_of(EVERY)
}

/// Where the workgroup count of a mesh draw comes from.
enum Source {
    /// The count of a direct draw.
    Direct(u32),
    /// The record `{count, first}` of an indirect draw, as an offset in guest memory. None when the address lies
    /// outside it.
    Record(Option<u64>),
}

/// A mesh draw of the frame.
struct Entry {
    pass: usize,
    ord: u32,
    kind: Kind,
    task: String,
    mesh: String,
    fragment: String,
    work: Source,
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
    events: Vec<Timed>,
    /// The knobs that were on when the frame was planned.
    knobs: Vec<&'static str>,
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

/// The offset in guest memory of the `bytes` at the GPU address `addr`, None when they lie outside it.
fn guest_offset(guest: &GuestSpace, addr: u64, bytes: u64) -> Option<u64> {
    let map = guest.address_map();
    let off = addr.checked_sub(map.gpu_base())?;
    (off.checked_add(bytes)? <= map.len()).then_some(off)
}

/// The plan of a frame, None when the category is off or the frame is not sampled.
pub(crate) fn plan(
    frame_no: u64,
    frame: &Frame,
    passes: &PassPlan,
    detail: &FrameDetail,
    ords: &[Ordinal],
    textures: &FastMap<TexId, Res>,
    guest: &GuestSpace,
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
    for (pi, p) in passes.passes.iter().enumerate() {
        if p.kind != PassKind::Render {
            continue;
        }
        let before = entries.len();
        for i in p.cmds.clone() {
            if !matches!(frame.cmds.get(i as usize), Some(Cmd::Draw { .. })) {
                continue;
            }
            let Some(ord) = ords.get(i as usize).map(|o| o.draw) else {
                continue;
            };
            let Some(d) = detail.draws.get(ord as usize).filter(|d| d.args.is_mesh()) else {
                continue;
            };
            let (kind, work) = match d.args {
                DrawArgs::MeshTasksIndirect { indirect } => (
                    Kind::MeshIndirect,
                    Source::Record(guest_offset(guest, indirect, 4)),
                ),
                DrawArgs::MeshTasks { count, .. } => (Kind::Mesh, Source::Direct(count)),
                _ => continue,
            };
            entries.push(Entry {
                pass: pi,
                ord,
                kind,
                task: key(d.program.task.as_ref()),
                mesh: key(d.program.mesh.as_ref()),
                fragment: key(d.program.fragment.as_ref()),
                work,
            });
        }
        if entries.len() > before {
            let mut info = PassInfo {
                index: pi,
                colours: Vec::new(),
                depth: None,
            };
            for a in passes.attachments_of(p) {
                match a.slot {
                    Slot::Color(_) => info.colours.push(att(a.tex)),
                    Slot::Depth => info.depth = Some(att(a.tex)),
                }
            }
            infos.push(info);
        }
    }
    let with_mesh: BTreeSet<usize> = entries.iter().map(|e| e.pass).collect();
    let mesh_of = |pi: usize| {
        if with_mesh.contains(&pi) {
            entries.iter().filter(|e| e.pass == pi).count() as u32
        } else {
            0
        }
    };
    let events = timeline::collect(frame, passes, detail, ords, &mesh_of);
    Some(Plan {
        frame: frame_no,
        passes: infos,
        live: Mutex::new((0..entries.len()).map(|_| Live::default()).collect()),
        entries,
        events,
        knobs: crate::diag::knobs_on(),
        base: 0,
        limit: 0,
        at: FastMap::default(),
        with_offsets: FastSet::default(),
    })
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
    /// count of an offset index of the plan and `record` the workgroup count at an offset in guest memory. Runs after
    /// the GPU finished the frame.
    pub(crate) fn report(
        &self,
        shared: &dyn Fn(u32) -> bool,
        samples: &dyn Fn(u64) -> Option<u64>,
        record: &dyn Fn(u64) -> Option<u32>,
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
                let work = match e.work {
                    Source::Direct(n) => Work::Groups(n),
                    Source::Record(at) => at.and_then(record).map_or(Work::Unread, Work::Groups),
                };
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
                    work,
                }
            })
            .collect();
        text::lines(self.frame, &self.passes, &draws, &self.events, &self.knobs)
    }
}
