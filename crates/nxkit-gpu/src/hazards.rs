//! What the passes of a frame touch beyond the texture lists of the analysis: the stages that bind each texture, and the
//! buffers (storage, uniform, vertex, index, indirect and copy buffers) by range of guest address.
//!
//! The barrier planner (`barriers.rs`) asks for a [`PassUse`] per pass. The buffers are tracked by GPU address, as
//! ranges, because the analysis has no buffer objects. Reads of uniform, vertex and index buffers are rounded to
//! 4 KiB pages: the walk binds thousands of them and an exact range per bind costs more than it saves.

use std::collections::BTreeMap;

use nxkit_metal::Stages;
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{PassKind, PassPlan};
use nxkit_nvn::resource::TexId;

use crate::detail::{CopyKind, DrawArgs, FrameDetail, Stage, UboDetail};
use crate::draws::{FastMap, Ordinal};
use crate::programs::{BindKind, Programs};
use crate::snapshot::EncodeView;

/// The page size that reads of uniform, vertex and index buffers are rounded to.
const PAGE: u64 = 4096;

/// How far an index buffer of an indirect draw is assumed to reach (the arguments do not say).
const INDEX_REACH: u64 = 16 << 20;

/// A range of guest address that a pass reads or writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufUse {
    /// The first byte.
    pub(crate) lo: u64,
    /// One past the last byte.
    pub(crate) hi: u64,
    /// The stages of the pass that touch it.
    pub(crate) stages: Stages,
    /// True when the pass may write the range.
    pub(crate) write: bool,
}

/// What one pass touches besides what the analysis lists.
#[derive(Debug, Default)]
pub(crate) struct PassUse {
    /// The stages in which the draws of the pass sample a texture. A sampled texture that is missing here is
    /// assumed to be read in every stage of the pass.
    pub(crate) tex: Vec<(TexId, Stages)>,
    /// The buffer ranges, sorted by start, overlapping ranges of the same kind merged.
    pub(crate) bufs: Vec<BufUse>,
}

/// The stage of a shader stage.
fn stage_of(s: Stage) -> Stages {
    match s {
        Stage::Vertex => Stages::VERTEX,
        Stage::Fragment => Stages::FRAGMENT,
        Stage::Compute => Stages::DISPATCH,
        Stage::Task => Stages::OBJECT,
        Stage::Mesh => Stages::MESH,
    }
}

/// Rounds a range out to whole pages.
fn pages(addr: u64, size: u64) -> (u64, u64) {
    let lo = addr & !(PAGE - 1);
    let hi = addr.saturating_add(size.max(1)).saturating_add(PAGE - 1) & !(PAGE - 1);
    (lo, hi.max(lo + PAGE))
}

/// Sorts the ranges and merges the overlapping ones that are the same kind of access.
fn merge(mut v: Vec<BufUse>) -> Vec<BufUse> {
    v.sort_by_key(|u| (u.write, u.lo));
    let mut out: Vec<BufUse> = Vec::with_capacity(v.len());
    for u in v {
        match out.last_mut() {
            Some(l) if l.write == u.write && u.lo <= l.hi => {
                l.hi = l.hi.max(u.hi);
                l.stages = l.stages | u.stages;
            }
            _ => out.push(u),
        }
    }
    out
}

/// The `sbuf` bindings of a program: slot and whether the shader may write.
type SbufRw = Vec<(u32, bool)>;

/// The uses of every pass of the frame.
pub(crate) fn pass_uses(
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[Ordinal],
    programs: &Programs,
    view: &dyn EncodeView,
) -> Vec<PassUse> {
    let mut handles: FastMap<u64, Option<TexId>> = FastMap::default();
    // The `sbuf` bindings per program key address and kind of reading, so a key is looked up once a frame.
    let mut rw: FastMap<(usize, bool), SbufRw> = FastMap::default();
    let mut sbuf_rw = |key: &str, vertex: bool, slot: u32| -> bool {
        let list = rw.entry((key.as_ptr() as usize, vertex)).or_insert_with(|| {
            programs.stage(key, vertex).map_or_else(
                |_| Vec::new(),
                |p| {
                    p.binds
                        .iter()
                        .filter(|b| b.kind == BindKind::Sbuf)
                        .map(|b| (b.reg, b.rw))
                        .collect()
                },
            )
        });
        // A binding the reflection does not name counts as written.
        list.iter().find(|(r, _)| *r == slot).is_none_or(|(_, w)| *w)
    };
    plan.passes
        .iter()
        .map(|p| {
            let mut out = PassUse::default();
            let mut bufs: Vec<BufUse> = Vec::new();
            let mut tex: FastMap<TexId, Stages> = FastMap::default();
            match p.kind {
                PassKind::Render => {
                    let first = ords.get(p.cmds.start as usize).map_or(0, |o| o.draw) as usize;
                    let n = p
                        .cmds
                        .clone()
                        .filter(|i| matches!(frame.cmds.get(*i as usize), Some(Cmd::Draw { .. })))
                        .count();
                    for d in detail.draws.get(first..first + n).unwrap_or(&[]) {
                        for t in d.textures.iter().filter(|t| t.handle != 0) {
                            let id = *handles
                                .entry(t.handle)
                                .or_insert_with(|| view.resolve(t.handle).ok().map(|(s, _)| s.texture));
                            if let Some(id) = id {
                                let e = tex.entry(id).or_default();
                                *e = *e | stage_of(t.stage);
                            }
                        }
                        let mesh_stage = if d.program.task.is_some() { Stages::OBJECT } else { Stages::MESH };
                        // Vertex and index buffers.
                        for s in &d.streams {
                            let reach = match (d.args, s.stride, s.divisor) {
                                (DrawArgs::Arrays { first, count, .. }, st, 0) if st > 0 => {
                                    (u64::from(first) + u64::from(count)) * u64::from(st)
                                }
                                (_, 0, _) => s.size.min(64),
                                _ => s.size,
                            };
                            let (lo, hi) = pages(s.address, reach.min(s.size.max(1)));
                            bufs.push(BufUse { lo, hi, stages: Stages::VERTEX, write: false });
                        }
                        let index = |address: u64, bytes: u64| {
                            let (lo, hi) = pages(address, bytes);
                            BufUse { lo, hi, stages: Stages::VERTEX, write: false }
                        };
                        let args = |address: u64, bytes: u64, stage: Stages| BufUse {
                            lo: address,
                            hi: address.saturating_add(bytes),
                            stages: stage,
                            write: false,
                        };
                        match d.args {
                            DrawArgs::Indexed { index_type, address, count, .. } => {
                                bufs.push(index(address, u64::from(count) << index_type.min(2)));
                            }
                            DrawArgs::ArraysIndirect { indirect } => {
                                bufs.push(args(indirect, 16, Stages::VERTEX));
                            }
                            DrawArgs::IndexedIndirect { address, indirect, .. } => {
                                bufs.push(index(address, INDEX_REACH));
                                bufs.push(args(indirect, 20, Stages::VERTEX));
                            }
                            DrawArgs::MultiIndirectCount { index: ix, indirect, count, max, stride } => {
                                if let Some((_, a)) = ix {
                                    bufs.push(index(a, INDEX_REACH));
                                }
                                let each = if stride == 0 { 20 } else { stride };
                                bufs.push(args(indirect, each * u64::from(max.min(4096)), Stages::VERTEX));
                                bufs.push(args(count, 4, Stages::VERTEX));
                            }
                            DrawArgs::MeshTasksIndirect { indirect } => {
                                bufs.push(args(indirect, 8, mesh_stage));
                            }
                            DrawArgs::Arrays { .. } | DrawArgs::MeshTasks { .. } => {}
                        }
                        // Uniform and storage buffers, by the stage of the binding.
                        for u in d.ubos.iter().filter(|u| u.snapshot.is_none()) {
                            let (lo, hi) = pages(u.address, u64::from(u.size));
                            bufs.push(BufUse { lo, hi, stages: stage_of(u.stage), write: false });
                        }
                        for s in &d.sbufs {
                            let key = match s.stage {
                                Stage::Vertex => Some((&*d.program.vertex, true)),
                                Stage::Fragment => d.program.fragment.as_deref().map(|k| (k, false)),
                                Stage::Task => d.program.task.as_deref().map(|k| (k, false)),
                                Stage::Mesh => d.program.mesh.as_deref().map(|k| (k, false)),
                                Stage::Compute => None,
                            };
                            let write = key.is_none_or(|(k, v)| sbuf_rw(k, v, s.slot));
                            bufs.push(sbuf(s, stage_of(s.stage), write));
                        }
                    }
                }
                PassKind::Compute => {
                    for i in p.cmds.clone() {
                        if !matches!(frame.cmds.get(i as usize), Some(Cmd::Dispatch { .. })) {
                            continue;
                        }
                        let ord = ords.get(i as usize).map_or(0, |o| o.dispatch) as usize;
                        let Some(d) = detail.dispatches.get(ord) else { continue };
                        for u in d.ubos.iter().filter(|u| u.snapshot.is_none()) {
                            let (lo, hi) = pages(u.address, u64::from(u.size));
                            bufs.push(BufUse { lo, hi, stages: Stages::DISPATCH, write: false });
                        }
                        for s in &d.sbufs {
                            let write = sbuf_rw(&d.program, false, s.slot);
                            bufs.push(sbuf(s, Stages::DISPATCH, write));
                        }
                    }
                }
                PassKind::Copy => {
                    for i in p.cmds.clone() {
                        if !matches!(frame.cmds.get(i as usize), Some(Cmd::Copy { .. })) {
                            continue;
                        }
                        let ord = ords.get(i as usize).map_or(0, |o| o.copy) as usize;
                        let Some(c) = detail.copies.get(ord) else { continue };
                        if !matches!(c.kind, CopyKind::BufferToTexture | CopyKind::TextureToBuffer) || c.buffer == 0 {
                            continue;
                        }
                        // The bytes of the buffer side: the strides when the game set them, otherwise 16 bytes a
                        // texel, which is the widest format.
                        let [w, h, d] = c.size.map(|v| u64::from(v.max(1)));
                        let tight = w * h * d * 16;
                        let rows = if c.row_stride > 0 { c.row_stride * h * d } else { 0 };
                        let layers = if c.image_stride > 0 { c.image_stride * d } else { 0 };
                        let bytes = tight.max(rows).max(layers);
                        bufs.push(BufUse {
                            lo: c.buffer,
                            hi: c.buffer.saturating_add(bytes),
                            stages: Stages::BLIT,
                            write: c.kind == CopyKind::TextureToBuffer,
                        });
                    }
                }
                _ => {}
            }
            out.tex = tex.into_iter().collect();
            out.bufs = merge(bufs);
            out
        })
        .collect()
}

/// A storage buffer binding as a use.
fn sbuf(s: &UboDetail, stages: Stages, write: bool) -> BufUse {
    BufUse {
        lo: s.address,
        hi: s.address.saturating_add(u64::from(s.size).max(1)),
        stages,
        write,
    }
}

/// How far the last writer or reader of each stage got, for a range: the global index of its pass, or `NO`.
pub(crate) type Last = [i64; 6];

/// "No pass".
pub(crate) const NO: i64 = i64::MIN;

/// Disjoint ranges of guest address, each with the last pass per stage that touched it.
///
/// Adding a range merges every range it overlaps or touches into one that takes the later pass of each stage. That
/// makes the state coarser, never wrong: it can only ask for more barriers.
#[derive(Debug, Default)]
pub(crate) struct Ranges {
    map: BTreeMap<u64, (u64, Last)>,
}

impl Ranges {
    /// The entries that overlap `[lo, hi)`.
    pub(crate) fn overlapping(&self, lo: u64, hi: u64) -> impl Iterator<Item = (u64, u64, &Last)> {
        let from = self.map.range(..=lo).next_back().map_or(0, |(k, _)| *k);
        self.map
            .range(from..hi)
            .filter(move |(_, (e, _))| *e > lo)
            .map(|(k, (e, l))| (*k, *e, l))
    }

    /// Notes that pass `at` touched `[lo, hi)` in `stages`.
    pub(crate) fn add(&mut self, lo: u64, hi: u64, stages: Stages, at: i64) {
        let (mut lo, mut hi) = (lo, hi.max(lo + 1));
        let mut last: Last = [NO; 6];
        // Touching ranges merge too, so a run of neighbouring pages stays one entry.
        let from = self.map.range(..=lo).next_back().map_or(0, |(k, _)| *k);
        let hits: Vec<u64> = self
            .map
            .range(from..=hi)
            .filter(|(_, (e, _))| *e >= lo)
            .map(|(k, _)| *k)
            .collect();
        for k in hits {
            if let Some((e, l)) = self.map.remove(&k) {
                lo = lo.min(k);
                hi = hi.max(e);
                for (a, b) in last.iter_mut().zip(l) {
                    *a = (*a).max(b);
                }
            }
        }
        for (i, s) in Stages::EACH.iter().enumerate() {
            if stages.contains(*s) {
                last[i] = last[i].max(at);
            }
        }
        self.map.insert(lo, (hi, last));
    }

    /// The number of entries.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_merge_what_touches_and_find_what_overlaps() {
        let mut r = Ranges::default();
        r.add(0x1000, 0x2000, Stages::DISPATCH, 3);
        r.add(0x2000, 0x3000, Stages::FRAGMENT, 5);
        assert_eq!(r.len(), 1);
        r.add(0x9000, 0xa000, Stages::BLIT, 7);
        assert_eq!(r.overlapping(0x2800, 0x2900).count(), 1);
        assert_eq!(r.overlapping(0x4000, 0x5000).count(), 0);
        let (_, _, l) = r.overlapping(0x1000, 0x1001).next().unwrap();
        assert_eq!(l[Stages::DISPATCH.index()], 3);
        assert_eq!(l[Stages::FRAGMENT.index()], 5);
    }

    #[test]
    fn pages_round_out() {
        assert_eq!(pages(0x1010, 8), (0x1000, 0x2000));
        assert_eq!(pages(0x1ff8, 16), (0x1000, 0x3000));
    }
}
