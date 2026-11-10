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
//!
//! The guest buffers of a slot come from the pool snapshot of the frame. An entry keeps the buffer and the id of every
//! pool it named, so the buffers live as long as the ICB. It is built again when one of those pools is gone from the
//! snapshot of a later frame, or was registered again under another id or range.

use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::Arc;

use crate::draws::{FastMap, FastSet};

use nxkit_metal::{GuestMemory, IcbBuffer, IcbCommand, IcbDesc, IndirectCommandBuffer, Inherit};

use crate::bindless::Arena;
use crate::detail::{DrawDetail, RecordingRun};
use crate::draws::{Geom, PassCtx, Shared, prepare};
use crate::guestpools::GuestPools;
use crate::pools::{Backing, Loc, PoolBuffer, PoolSnapshot};
use crate::stats::Skip;

#[cfg(test)]
mod tests;

const DRAW_ARENA_BYTES: u64 =
    2 * (crate::bindless::DRV_SIZE as u64 + 2048) + crate::pull::BYTES as u64 + 16;

/// The pools that one ICB named: for each the id, the range and the buffer. The buffers stay alive with the ICB. Needs
/// no Metal object, so the checks on it run in tests.
pub(crate) struct PoolRefs<B: Backing> {
    /// The generation of the snapshot in which every pool was last seen to be current.
    generation: u64,
    pools: Vec<Named<B>>,
    /// The buffers of the mirror runs that the ICB names. They stay alive with it; a run that grew is another buffer, and
    /// the signature of the ICB holds the real addresses, so such an ICB is built again.
    runs: Vec<B>,
}

struct Named<B: Backing> {
    id: u64,
    start: u64,
    end: u64,
    /// Held only to keep the buffer alive.
    _buf: Arc<PoolBuffer<B>>,
}

impl<B: Backing + Clone> PoolRefs<B> {
    /// No pools yet, taken at the generation of `snap`.
    pub(crate) fn new(snap: &PoolSnapshot<B>) -> Self {
        Self {
            generation: snap.generation(),
            pools: Vec::new(),
            runs: Vec::new(),
        }
    }

    /// Notes the pool of `loc` (once per pool) and gives its backing, or `None` when the pool has none.
    pub(crate) fn take(&mut self, snap: &PoolSnapshot<B>, loc: Loc) -> Option<B> {
        if loc.run != crate::pools::NO_RUN {
            // A range in the mirror: the buffer of its run, which the ICB keeps alive.
            let mem = snap.overlay()?.runs().get(loc.run as usize)?.mem.clone();
            if !self.runs.iter().any(|m| m.real_base() == mem.real_base()) {
                self.runs.push(mem.clone());
            }
            return Some(mem);
        }
        let e = snap.entries().get(loc.pool as usize)?;
        let mem = e.buf.mem()?.clone();
        if !self.pools.iter().any(|n| n.id == e.id) {
            self.pools.push(Named {
                id: e.id,
                start: e.start,
                end: e.end,
                _buf: Arc::clone(&e.buf),
            });
        }
        Some(mem)
    }

    /// True when every pool is in `snap` under the same id at the same range. The snapshot only changes its
    /// generation when a pool is registered or unregistered, so the same generation as at the last check means
    /// nothing changed and no id is looked up. Otherwise each id is looked up by its start; the generation is kept
    /// when all pools are still there, so the lookups run once per change and not once per frame.
    pub(crate) fn current(&mut self, snap: &PoolSnapshot<B>) -> bool {
        if self.generation == snap.generation() {
            return true;
        }
        let es = snap.entries();
        let same = self.pools.iter().all(|n| {
            let i = es.partition_point(|e| e.start < n.start);
            es.get(i)
                .is_some_and(|e| e.start == n.start && e.end == n.end && e.id == n.id)
        });
        if same {
            self.generation = snap.generation();
        }
        same
    }

    /// True when the ICB names one of the pools `ids`.
    pub(crate) fn names_any(&self, ids: &[u64]) -> bool {
        self.pools.iter().any(|n| ids.contains(&n.id))
    }
}

/// Drops the entries of `map` whose pools include one of `ids`.
fn forget<K, V, B: Backing + Clone>(
    map: &mut FastMap<K, V>,
    ids: &[u64],
    refs: impl Fn(&V) -> &PoolRefs<B>,
) {
    map.retain(|_, v| !refs(v).names_any(ids));
}

struct Entry {
    icb: IndirectCommandBuffer,
    pools: PoolRefs<Arc<GuestMemory>>,
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
    /// By recording and table: the argument buffers of a draw with a `tex_pool` or `samp_pool` binding hold the address
    /// of the pool table of the batch slot that built them, and a later frame in another slot must not read an older
    /// copy of the table. So such an ICB is kept per slot (second key part: the address of the slot's texture table, at
    /// most one ICB per slot in flight), and any other ICB once (second key part 0).
    entries: FastMap<(u64, u64), Entry>,
    candidates: FastSet<u64>,
}

fn hash_draw(sh: &Shared<'_>, d: &DrawDetail, h: &mut impl Hasher) {
    d.program.hash(h);
    d.blend.hash(h);
    d.write_masks.hash(h);
    d.depth_stencil.hash(h);
    let r = &d.raster;
    (
        r.cull,
        r.front_face,
        r.polygon_mode,
        r.depth_clamp,
        r.alpha_to_coverage,
    )
        .hash(h);
    r.depth_bias.map(|b| b.map(f32::to_bits)).hash(h);
    d.topology.hash(h);
    d.attributes.hash(h);
    d.streams.hash(h);
    d.args.hash(h);
    d.ubos.hash(h);
    // A renamed range reads the copy of the batch slot, whose address differs from frame to frame.
    for s in &d.sbufs {
        sh.env.rename.resolve(s.address, s.size).hash(h);
    }
    // A range in the mirror has the address of its run's buffer, which is another one when the run grew.
    if sh.pools.overlay().is_some() {
        let real = |a: u64| sh.pools.resolve_start(a).ok().map(|l| (l.run, l.real));
        for u in d.ubos.iter().chain(&d.sbufs) {
            real(u.address).hash(h);
        }
        for s in &d.streams {
            real(s.address).hash(h);
        }
        if let crate::detail::DrawArgs::Indexed { address, .. } = d.args {
            real(address).hash(h);
        }
    }
    for t in &d.textures {
        t.hash(h);
        sh.env
            .handles
            .get(&t.handle)
            .map(|r| (r.texture, r.sampler))
            .hash(h);
    }
}

fn signature(sh: &Shared<'_>, pc: &PassCtx, draws: &[Arc<DrawDetail>]) -> u64 {
    let mut h = foldhash::fast::FixedState::default().build_hasher();
    (&pc.targets, pc.has_depth, pc.has_stencil).hash(&mut h);
    for d in draws {
        hash_draw(sh, d, &mut h);
    }
    h.finish()
}

/// True when the draws of a run can share one set of encoder state.
fn uniform(draws: &[Arc<DrawDetail>]) -> bool {
    let Some(first) = draws.first() else {
        return false;
    };
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
        self.entries.retain(|(h, _), _| keep.contains(h));
    }

    /// Forgets the ICB of a recording.
    pub(crate) fn drop_recording(&mut self, handle: u64) {
        self.entries.retain(|(h, _), _| *h != handle);
        self.candidates.remove(&handle);
    }

    /// Forgets everything.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Forgets the ICBs that name one of the pools `ids`. A pool that went away also fails the check in `get`; this
    /// frees the ICB and its buffers at once.
    // The per-pool mode calls it when a new snapshot generation unregistered pools (package P7).
    #[allow(dead_code)]
    pub(crate) fn forget_pools(&mut self, ids: &[u64]) {
        forget(&mut self.entries, ids, |e| &e.pools);
    }

    /// The number of ICBs held.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The ICB of a run, built or built again when needed. `None` when the run gets no ICB and the caller encodes
    /// its draws one by one.
    pub(crate) fn get(
        &mut self,
        sh: &Shared<'_>,
        pc: &PassCtx,
        run: &RecordingRun,
        draws: &[Arc<DrawDetail>],
    ) -> Option<Built> {
        if !self.candidates.contains(&run.handle) || draws.len() < 2 || !uniform(draws) {
            return None;
        }
        let sig = signature(sh, pc, draws);
        for key in [(run.handle, 0), (run.handle, sh.env.pools.0)] {
            if let Some(e) = self.entries.get_mut(&key) {
                if e.signature == sig && e.pools.current(sh.pools) {
                    return Some(Built {
                        icb: e.icb.clone(),
                        slots: e.slots,
                        skipped: e.skipped.clone(),
                        rebuilt: false,
                    });
                }
                // Another signature, or a pool it named is gone: the old buffer is of no use.
                self.entries.remove(&key);
            }
        }
        let (e, pooled) = build(sh, pc, draws, sig)?;
        let b = Built {
            icb: e.icb.clone(),
            slots: e.slots,
            skipped: e.skipped.clone(),
            rebuilt: true,
        };
        let table = if pooled { sh.env.pools.0 } else { 0 };
        self.entries.insert((run.handle, table), e);
        Some(b)
    }
}

fn build(
    sh: &Shared<'_>,
    pc: &PassCtx,
    draws: &[Arc<DrawDetail>],
    signature: u64,
) -> Option<(Entry, bool)> {
    let mut arena = Arena::new(sh.ctx, DRAW_ARENA_BYTES * draws.len() as u64).ok()?;
    let mut cmds: Vec<Vec<IcbCommand>> = Vec::with_capacity(draws.len());
    let mut skipped = Vec::new();
    let mut pools = PoolRefs::new(sh.pools);
    let mut pooled = false;
    for d in draws {
        match slot_commands(sh, pc, d, &mut arena, &mut pools) {
            Ok((c, p)) => {
                cmds.push(c);
                pooled |= p;
            }
            Err(why) => skipped.push(why),
        }
    }
    if cmds.is_empty() {
        return None;
    }
    let desc = IcbDesc {
        max_commands: cmds.len() as u32,
        inherit: Inherit {
            cull: true,
            ..Inherit::NOTHING
        },
        max_vertex_buffers: 31,
        max_fragment_buffers: 31,
    };
    let icb = sh.ctx.new_indirect_command_buffer(&desc).ok()?;
    for (slot, c) in cmds.iter().enumerate() {
        icb.encode(slot as u32, c).ok()?;
    }
    let entry = Entry {
        icb,
        pools,
        signature,
        slots: cmds.len() as u32,
        skipped,
    };
    Some((entry, pooled))
}

/// The buffer of the pool that holds a guest address and the offset in it. `None` (an address outside guest memory) or
/// a pool without a buffer is `Skip::Buffer`.
fn icb_buffer(
    snap: &GuestPools,
    pools: &mut PoolRefs<Arc<GuestMemory>>,
    loc: Option<Loc>,
) -> Result<IcbBuffer, Skip> {
    let loc = loc.ok_or(Skip::Buffer)?;
    let guest = pools.take(snap, loc).ok_or(Skip::Buffer)?;
    Ok(IcbBuffer {
        guest,
        offset: loc.offset,
    })
}

fn slot_commands(
    sh: &Shared<'_>,
    pc: &PassCtx,
    d: &DrawDetail,
    arena: &mut Arena,
    pools: &mut PoolRefs<Arc<GuestMemory>>,
) -> Result<(Vec<IcbCommand>, bool), Skip> {
    let p = prepare(sh, pc, d, arena, true)?;
    let data = |addr: u64| (arena.buffer.clone(), arena.offset_of(addr));
    let mut c = vec![IcbCommand::SetPipeline(p.pipeline.clone())];
    let (b, offset) = data(p.vertex_args);
    c.push(IcbCommand::SetVertexData {
        index: 0,
        buffer: b,
        offset,
    });
    if let Some(a) = p.fragment_args {
        let (b, offset) = data(a);
        c.push(IcbCommand::SetFragmentData {
            index: 0,
            buffer: b,
            offset,
        });
    }
    for ((index, addr), loc) in p.streams.iter().zip(&p.stream_locs) {
        // The default vertex and the layout block of a pulled draw are in the arena; the streams in guest memory.
        if *index == crate::translate::DEFAULT_VERTEX_SLOT
            || p.pulled && *index == crate::pull::SLOT
        {
            let (buffer, offset) = data(*addr);
            c.push(IcbCommand::SetVertexData {
                index: *index,
                buffer,
                offset,
            });
        } else {
            c.push(IcbCommand::SetVertexBuffer {
                index: *index,
                buffer: icb_buffer(sh.pools, pools, *loc)?,
            });
        }
    }
    c.push(IcbCommand::SetDepthStencil(p.depth_stencil.clone()));
    c.push(IcbCommand::SetDepthBias(p.bias));
    c.push(IcbCommand::SetDepthClip(p.clip));
    c.push(IcbCommand::SetWinding(p.winding));
    c.push(IcbCommand::SetFillMode(p.fill));
    c.push(match p.geom {
        Geom::Arrays {
            primitive,
            first,
            count,
            instances,
            base_instance,
        } => IcbCommand::Draw {
            primitive,
            first_vertex: first,
            vertex_count: count,
            instance_count: instances,
            base_instance,
        },
        Geom::Indexed {
            primitive,
            index_type,
            address: _,
            loc,
            count,
            instances,
            base_vertex,
            base_instance,
        } => IcbCommand::DrawIndexed {
            primitive,
            index_type,
            indices: icb_buffer(sh.pools, pools, loc)?,
            index_count: count,
            instance_count: instances,
            base_vertex,
            base_instance,
        },
        // `prepare` refuses the indirect and mesh draws for an ICB.
        Geom::Indirect { .. }
        | Geom::IndexedIndirect { .. }
        | Geom::MultiIndirect { .. }
        | Geom::Mesh { .. }
        | Geom::MeshIndirect { .. } => return Err(Skip::Encode),
    });
    Ok((c, p.pooled))
}
