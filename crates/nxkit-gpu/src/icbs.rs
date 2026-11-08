//! One indirect command buffer per reused recording (R17).
//!
//! `RecCache::icb_candidates` of `nxkit-nvn` names the recordings that the game replays often and that hold draws.
//! The recorder reports each replay as a [`RecordingRun`](crate::detail::RecordingRun). When a candidate run lies
//! inside one render pass, the encoder builds an indirect command buffer from its draws once and later frames run it
//! with `executeCommandsInBuffer`.
//!
//! A slot of the buffer is one draw and carries its pipeline, its argument buffers, its vertex and index buffers, its
//! depth and stencil state and its rasteriser state. The cull mode comes from the encoder. The viewport, the scissor
//! rectangle, the stencil reference and the blend colour cannot be part of an ICB. The encoder sets them from the
//! first draw of the run before it executes the buffer, so a run is only eligible when all its draws agree on them.
//!
//! A built buffer is valid for a signature. The signature hashes everything that went into the slots: the draw
//! details, the resolved texture and sampler ids and the attachment formats of the pass. Another signature builds
//! the buffer again. `Gpu::recording_dropped` forgets a buffer when the cache dropped its summary.

use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::Arc;

use crate::draws::{FastMap, FastSet};

use nxkit_metal::{IcbBuffer, IcbCommand, IcbDesc, IndirectCommandBuffer, Inherit};

use crate::bindless::Arena;
use crate::detail::{DrawDetail, RecordingRun};
use crate::draws::{Geom, PassCtx, Shared, prepare};
use crate::stats::Skip;

const DRAW_ARENA_BYTES: u64 = 2 * (crate::bindless::DRV_SIZE as u64 + 2048);

struct Entry {
    icb: IndirectCommandBuffer,
    signature: u64,
    slots: u32,
    skipped: Vec<Skip>,
}

/// An ICB that is ready to execute.
pub(crate) struct Built {
    /// The buffer.
    pub(crate) icb: IndirectCommandBuffer,
    /// The number of commands in it.
    pub(crate) slots: u32,
    /// The draws that did not go into the buffer, one reason each.
    pub(crate) skipped: Vec<Skip>,
    /// True when this call encoded the buffer.
    pub(crate) rebuilt: bool,
}

/// The ICBs of all candidate recordings.
#[derive(Default)]
pub(crate) struct Icbs {
    entries: FastMap<u64, Entry>,
    candidates: FastSet<u64>,
}

fn hash_draw(sh: &Shared<'_>, d: &DrawDetail, h: &mut impl Hasher) {
    d.program.hash(h);
    d.blend.hash(h);
    d.write_masks.hash(h);
    d.depth_stencil.hash(h);
    let r = &d.raster;
    (r.cull, r.front_face, r.polygon_mode, r.depth_clamp, r.alpha_to_coverage).hash(h);
    r.depth_bias.map(|b| b.map(f32::to_bits)).hash(h);
    d.topology.hash(h);
    d.attributes.hash(h);
    d.streams.hash(h);
    d.args.hash(h);
    d.ubos.hash(h);
    for t in &d.textures {
        t.hash(h);
        sh.env.handles.get(&t.handle).map(|r| (r.texture, r.sampler)).hash(h);
    }
}

fn signature(sh: &Shared<'_>, pc: &PassCtx, draws: &[DrawDetail]) -> u64 {
    let mut h = DefaultHasher::new();
    (&pc.targets, pc.has_depth, pc.has_stencil).hash(&mut h);
    for d in draws {
        hash_draw(sh, d, &mut h);
    }
    h.finish()
}

/// True when the draws of a run can share one set of encoder state.
fn uniform(draws: &[DrawDetail]) -> bool {
    let Some(first) = draws.first() else { return false };
    draws.iter().all(|d| {
        d.dynamic == first.dynamic
            && d.raster.cull == first.raster.cull
            && d.ubos.iter().all(|u| u.snapshot.is_none())
    })
}

impl Icbs {
    /// Replaces the set of recordings that may get an ICB.
    pub(crate) fn set_candidates(&mut self, handles: impl IntoIterator<Item = u64>) {
        self.candidates = handles.into_iter().collect();
        let keep = &self.candidates;
        self.entries.retain(|h, _| keep.contains(h));
    }

    /// Forgets the ICB of a recording.
    pub(crate) fn drop_recording(&mut self, handle: u64) {
        self.entries.remove(&handle);
        self.candidates.remove(&handle);
    }

    /// Forgets everything.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// The number of ICBs held.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The ICB of a run, built or built again when needed. `None` when the run gets no ICB and the caller encodes
    /// its draws one by one.
    pub(crate) fn get(&mut self, sh: &Shared<'_>, pc: &PassCtx, run: &RecordingRun, draws: &[DrawDetail]) -> Option<Built> {
        if !self.candidates.contains(&run.handle) || draws.len() < 2 || !uniform(draws) {
            return None;
        }
        let sig = signature(sh, pc, draws);
        if let Some(e) = self.entries.get(&run.handle).filter(|e| e.signature == sig) {
            return Some(Built { icb: e.icb.clone(), slots: e.slots, skipped: e.skipped.clone(), rebuilt: false });
        }
        let e = build(sh, pc, draws, sig)?;
        let b = Built { icb: e.icb.clone(), slots: e.slots, skipped: e.skipped.clone(), rebuilt: true };
        self.entries.insert(run.handle, e);
        Some(b)
    }
}

fn build(sh: &Shared<'_>, pc: &PassCtx, draws: &[DrawDetail], signature: u64) -> Option<Entry> {
    let mut arena = Arena::new(sh.ctx, DRAW_ARENA_BYTES * draws.len() as u64).ok()?;
    let mut cmds: Vec<Vec<IcbCommand>> = Vec::with_capacity(draws.len());
    let mut skipped = Vec::new();
    for d in draws {
        match slot_commands(sh, pc, d, &mut arena) {
            Ok(c) => cmds.push(c),
            Err(why) => skipped.push(why),
        }
    }
    if cmds.is_empty() {
        return None;
    }
    let desc = IcbDesc {
        max_commands: cmds.len() as u32,
        inherit: Inherit { cull: true, ..Inherit::NOTHING },
        max_vertex_buffers: 31,
        max_fragment_buffers: 31,
    };
    let icb = sh.ctx.new_indirect_command_buffer(&desc).ok()?;
    for (slot, c) in cmds.iter().enumerate() {
        icb.encode(slot as u32, c).ok()?;
    }
    Some(Entry { icb, signature, slots: cmds.len() as u32, skipped })
}

fn slot_commands(sh: &Shared<'_>, pc: &PassCtx, d: &DrawDetail, arena: &mut Arena) -> Result<Vec<IcbCommand>, Skip> {
    let p = prepare(sh, pc, d, arena, true)?;
    let buffer = |addr: u64| -> Result<IcbBuffer, Skip> {
        Ok(IcbBuffer { guest: sh.guest.clone(), offset: sh.guest_offset(addr, 1).ok_or(Skip::Buffer)? })
    };
    let data = |addr: u64| (arena.buffer.clone(), arena.offset_of(addr));
    let mut c = vec![IcbCommand::SetPipeline(p.pipeline.clone())];
    let (b, offset) = data(p.vertex_args);
    c.push(IcbCommand::SetVertexData { index: 0, buffer: b, offset });
    if let Some(a) = p.fragment_args {
        let (b, offset) = data(a);
        c.push(IcbCommand::SetFragmentData { index: 0, buffer: b, offset });
    }
    for (index, addr) in &p.streams {
        c.push(IcbCommand::SetVertexBuffer { index: *index, buffer: buffer(*addr)? });
    }
    c.push(IcbCommand::SetDepthStencil(p.depth_stencil.clone()));
    c.push(IcbCommand::SetDepthBias(p.bias));
    c.push(IcbCommand::SetDepthClip(p.clip));
    c.push(IcbCommand::SetWinding(p.winding));
    c.push(IcbCommand::SetFillMode(p.fill));
    c.push(match p.geom {
        Geom::Arrays { primitive, first, count, instances, base_instance } => IcbCommand::Draw {
            primitive,
            first_vertex: first,
            vertex_count: count,
            instance_count: instances,
            base_instance,
        },
        Geom::Indexed { primitive, index_type, address, count, instances, base_vertex, base_instance } => {
            IcbCommand::DrawIndexed {
                primitive,
                index_type,
                indices: buffer(address)?,
                index_count: count,
                instance_count: instances,
                base_vertex,
                base_instance,
            }
        }
    });
    Ok(c)
}
