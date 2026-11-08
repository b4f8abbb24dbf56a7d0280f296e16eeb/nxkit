//! The public entry of the crate: [`Gpu`] turns a held NVN frame into Metal 4 work.
//!
//! One call of [`Gpu::encode_frame`] does these steps in order.
//!
//! 1. It finds out how each texture is used (attachment, memoryless, CPU read) and makes the Metal textures at the
//!    frame scale.
//! 2. It resolves the texture handles of all draws and dispatches into resource IDs and fills the pool tables.
//! 3. It encodes the uploads of the textures that the guest wrote into the first command buffer.
//! 4. It plans the barriers over the whole frame.
//! 5. It cuts the passes into runs and encodes each run into its own command buffer on its own thread.
//! 6. It commits the command buffers in order.

use crate::draws::{FastMap, FastSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use nxkit_metal::{
    ArchiveConfig, BatchRing, CommandBuffer, CommandBufferStats, Context, DataBuffer, GobOrder,
    GuestMemory, MetalError, Metrics, Result, Scratch, Stages, SubmittedBatch, Texture,
};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{PassKind, PassPlan};
use nxkit_nvn::resource::TexId;

use crate::barriers::{BarrierState, touch};
use crate::bindless::{Bindless, Env, Handles, Resolved};
use crate::copyback::CopyBack;
use crate::detail::{DrawArgs, FrameDetail, RecordingRun};
use crate::draws::{Shared, ordinals};
use crate::encode::{Tables, render_pass};
use crate::icbs::Icbs;
use crate::jobs;
use crate::passes::{compute_pass, cpu_read};
use crate::pipelines::Pipelines;
use crate::programs::Programs;
use crate::snapshot::EncodeView;
use crate::stats::{EncodeReport, EncodeStats, Skip};
use crate::textures::{Res, TextureStore, Use};
use crate::upload::{Dirty, Transfer};

/// The chunks of passes that the encoder makes for each thread (see `jobs::run_all`).
const CHUNKS_PER_THREAD: usize = 1;

/// What the caller chooses when it makes a [`Gpu`].
#[derive(Clone, Debug)]
pub struct GpuConfig {
    /// The directory with the Metal libraries and the `_b.json` files that the shader step wrote.
    pub shader_dir: PathBuf,
    /// The number of threads that encode passes. One means the calling thread alone.
    pub threads: usize,
    /// Where the pipeline cache loads an archive from, and whether it collects pipelines for one.
    pub archive: ArchiveConfig,
    /// The order of the 16 byte pieces in a GOB of the guest textures.
    pub gob_order: GobOrder,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            shader_dir: PathBuf::from("shaders"),
            threads: std::thread::available_parallelism()
                .map_or(2, |n| n.get())
                .min(6),
            archive: ArchiveConfig::default(),
            gob_order: GobOrder::default(),
        }
    }
}

/// The objects that every command buffer of a frame keeps alive until the GPU is done with it.
type Keep = (Vec<Texture>, Arc<DataBuffer>, Arc<DataBuffer>);

/// The frame encoder.
pub struct Gpu {
    ctx: Context,
    guest: Arc<GuestMemory>,
    programs: Programs,
    /// The texture types that each shader declares for its texture bindings, by program key (fragment and compute
    /// first, vertex second), for [`Gpu::fit_handles`].
    fit_decl: [FastMap<String, Vec<(u32, nxkit_metal::TextureKind)>>; 2],
    pipelines: Pipelines,
    bindless: Bindless,
    store: TextureStore,
    transfer: Transfer,
    icbs: Mutex<Icbs>,
    barriers: BarrierState,
    ring: BatchRing,
    threads: usize,
    last: Option<SubmittedBatch>,
    /// The batch before `last`, while it may still run: the frame that `wait_older` waits for.
    older: Option<SubmittedBatch>,
    pools: Vec<(u64, u64)>,
    copyback: CopyBack,
}

/// How each texture of the frame is used. A texture is memoryless only when every attachment use says so and no
/// command samples, copies, presents or reads it.
pub(crate) fn collect_uses(frame: &Frame, plan: &PassPlan) -> FastMap<TexId, Use> {
    let mut other: FastSet<TexId> = FastSet::default();
    let mut read_back: FastSet<TexId> = FastSet::default();
    for c in &frame.cmds {
        match *c {
            Cmd::Draw { reads, .. } => other.extend(frame.list(reads).iter().copied()),
            Cmd::Dispatch { reads, writes } => {
                other.extend(frame.list(reads).iter().copied());
                other.extend(frame.list(writes).iter().copied());
            }
            Cmd::Copy { src, dst } => other.extend(src.into_iter().chain(dst)),
            Cmd::Downsample { src, dst } => other.extend([src, dst]),
            Cmd::Present(id) => {
                other.insert(id);
            }
            Cmd::CpuRead(id) => {
                other.insert(id);
                read_back.insert(id);
            }
            _ => {}
        }
    }
    let mut uses: FastMap<TexId, Use> = FastMap::default();
    for a in &plan.attachments {
        let u = uses.entry(a.tex).or_insert(Use {
            attachment: true,
            memoryless: true,
            cpu_read: false,
        });
        u.memoryless &= a.memoryless && !other.contains(&a.tex);
    }
    for id in other {
        uses.entry(id).or_default();
    }
    for id in read_back {
        uses.entry(id).or_default().cpu_read = true;
    }
    uses
}

fn run_chunk(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    keep: &Arc<Keep>,
    range: Range<usize>,
) -> (EncodeStats, Vec<TexId>) {
    nxkit_metal::autorelease_pool(|| run_chunk_inner(sh, cb, scratch, keep, range))
}

fn run_chunk_inner(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    keep: &Arc<Keep>,
    range: Range<usize>,
) -> (EncodeStats, Vec<TexId>) {
    let mut stats = EncodeStats::default();
    let mut copied = Vec::new();
    cb.retain(keep.clone());
    let Ok(tables) = Tables::new(sh.ctx) else {
        stats.skipped[Skip::Encode as usize] += 1;
        return (stats, copied);
    };
    for index in range {
        let Some(pass) = sh.plan.passes.get(index) else {
            continue;
        };
        match pass.kind {
            PassKind::Render => render_pass(sh, cb, &tables, scratch, index, pass, &mut stats),
            PassKind::Compute | PassKind::Copy => {
                compute_pass(sh, cb, &tables, scratch, index, pass, &mut stats)
            }
            PassKind::CpuRead => copied.extend(cpu_read(sh, cb, scratch, index, pass, &mut stats)),
            PassKind::Sync | PassKind::Present => {}
        }
    }
    (stats, copied)
}

/// The nanoseconds from `from` to `to`.
fn nanos(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.duration_since(from).as_nanos()).unwrap_or(u64::MAX)
}

/// The texture types that the shader of `key` declares, as (slot, type). The list is cached by key and borrowed.
fn declared_views<'a>(
    decl: &'a mut [FastMap<String, Vec<(u32, nxkit_metal::TextureKind)>>; 2],
    programs: &crate::programs::Programs,
    key: &str,
    vertex: bool,
) -> &'a [(u32, nxkit_metal::TextureKind)] {
    let map = &mut decl[usize::from(vertex)];
    if !map.contains_key(key) {
        let v = programs
            .stage(key, vertex)
            .map(|p| {
                p.binds
                    .iter()
                    .filter(|b| b.kind == crate::programs::BindKind::Tex)
                    .filter_map(|b| b.dim.map(|k| (b.reg, k)))
                    .collect()
            })
            .unwrap_or_default();
        map.insert(key.to_owned(), v);
    }
    map.get(key).map_or(&[][..], Vec::as_slice)
}

impl Gpu {
    /// Makes the encoder with the default configuration. The shader directory is `shaders` in the working directory.
    pub fn new(ctx: &Context, guest: Arc<GuestMemory>) -> Result<Self> {
        Self::with_config(ctx, guest, GpuConfig::default())
    }

    /// Makes the encoder.
    pub fn with_config(ctx: &Context, guest: Arc<GuestMemory>, config: GpuConfig) -> Result<Self> {
        let threads = config.threads.max(1);
        let pipelines = Pipelines::new(ctx, &config.archive)?;
        let transfer = Transfer::new(ctx, pipelines.cache(), config.gob_order)?;
        Ok(Self {
            ctx: ctx.clone(),
            guest,
            programs: Programs::new(ctx, config.shader_dir),
            fit_decl: [FastMap::default(), FastMap::default()],
            pipelines,
            bindless: Bindless::new(ctx)?,
            store: TextureStore::new(ctx),
            transfer,
            icbs: Mutex::new(Icbs::default()),
            barriers: BarrierState::default(),
            ring: BatchRing::new(ctx, threads * CHUNKS_PER_THREAD + 1)?,
            threads,
            last: None,
            older: None,
            pools: Vec::new(),
            copyback: CopyBack::default(),
        })
    }

    /// Tells the encoder about a texture or sampler pool that the game made. The pool memory is part of the guest
    /// memory, so nothing is copied. The call checks that the range is inside it.
    pub fn register_pool(&mut self, gpu_addr: u64, len: u64) -> Result<()> {
        self.guest.address_map().host_address(gpu_addr, len)?;
        if !self.pools.contains(&(gpu_addr, len)) {
            self.pools.push((gpu_addr, len));
        }
        Ok(())
    }

    /// Tells the encoder about a memory pool of the game and its `NVNmemoryPoolFlags`, like
    /// [`Gpu::register_pool`]. A texture that a frame writes and whose storage lies in a pool the CPU can access is
    /// copied back into guest memory at the end of the frame (see `copyback`).
    pub fn register_memory_pool(&mut self, gpu_addr: u64, len: u64, flags: u32) -> Result<()> {
        self.register_pool(gpu_addr, len)?;
        self.copyback.add_pool(gpu_addr, len, flags);
        Ok(())
    }

    /// The game finalised the memory pool at `gpu_addr`. Its textures are no longer copied back.
    pub fn unregister_memory_pool(&mut self, gpu_addr: u64) {
        self.copyback.remove_pool(gpu_addr);
    }

    /// The pools that were registered, as (GPU address, length).
    pub fn pools(&self) -> &[(u64, u64)] {
        &self.pools
    }

    /// The game created or finalized a texture. The Metal texture of the id, if any, is stale and made again at the
    /// next use.
    pub fn texture_created(&mut self, id: TexId) {
        self.store.invalidate(id);
        self.barriers.forget(id);
        drop(self.store.take_retired());
    }

    /// The game destroyed a texture.
    pub fn texture_destroyed(&mut self, id: TexId) {
        self.texture_created(id);
    }

    /// Names the recordings that `RecCache::icb_candidates` found. Only runs of these recordings get an ICB.
    pub fn set_icb_candidates(&self, handles: impl IntoIterator<Item = u64>) {
        self.icbs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_candidates(handles);
    }

    /// The cache dropped the summary of a recording, so its ICB is built again at the next use.
    pub fn recording_dropped(&self, handle: u64) {
        self.icbs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drop_recording(handle);
    }

    /// The number of ICBs that are alive.
    pub fn icb_count(&self) -> usize {
        self.icbs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Forgets the programs and the ICBs, for example after the shader directory changed. The pipelines stay.
    pub fn reload_shaders(&self) {
        self.programs.clear();
        self.icbs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// The counters of the command buffers: CPU encode time and GPU time (R16).
    pub fn metrics(&self) -> Arc<Metrics> {
        self.ring.metrics()
    }

    /// The Metal texture that holds a texture of the game, as the last frame left it. None when no frame used it.
    pub fn texture(&self, id: TexId) -> Option<Texture> {
        self.store.get(id).map(|r| r.tex.clone())
    }

    /// The newest batch that went to the GPU, to wait for it without this lock.
    pub fn last_submitted(&self) -> Option<SubmittedBatch> {
        self.last.clone()
    }

    /// Waits until the GPU finished the batch before the newest one, and leaves the newest one running.
    pub fn wait_older(&mut self) -> Result<Option<CommandBufferStats>> {
        self.older.take().map(|b| b.wait()).transpose()
    }

    /// Waits until the GPU finished the last submitted frame. The result is `None` when nothing was submitted.
    pub fn wait(&mut self) -> Result<Option<CommandBufferStats>> {
        let r = self.last.take().map(|b| b.wait()).transpose();
        if r.is_ok() {
            self.copyback.idle();
        }
        r
    }

    fn resolve_handles(
        &mut self,
        view: &dyn EncodeView,
        detail: &FrameDetail,
        scale: f32,
        textures: &mut FastMap<TexId, Res>,
        stats: &mut EncodeStats,
    ) -> Handles {
        let mut out = Handles::default();
        let wanted = detail
            .draws
            .iter()
            .flat_map(|d| d.textures.iter())
            .chain(detail.dispatches.iter().flat_map(|d| d.textures.iter()))
            .map(|t| t.handle)
            // Handle 0 is a slot the game left empty. The binding reads it as the null texture.
            .filter(|&h| h != 0);
        // Each handle is resolved once a frame, also when it fails: a handle that does not resolve is bound by many
        // draws, and trying it again for each of them cost a millisecond in heavy frames.
        let mut seen: FastSet<u64> = FastSet::default();
        for handle in wanted {
            if !seen.insert(handle) {
                continue;
            }
            match self.resolve_one(view, handle, scale, textures, stats) {
                Ok((r, view)) => {
                    self.bindless.publish(handle, r);
                    out.map.insert(handle, r);
                    out.keep.push(view);
                }
                Err(why) => stats.skip(why),
            }
        }
        out
    }

    /// Makes the views that the shaders of the frame declare for their texture bindings when they differ from the
    /// view of the descriptor (a cube read as a cube array, a 2D array read as 2D). Metal reads a texture of another
    /// type than the argument declares as one fixed value.
    fn fit_handles(
        &mut self,
        view: &dyn EncodeView,
        detail: &FrameDetail,
        textures: &FastMap<TexId, Res>,
        handles: &mut Handles,
        stats: &mut EncodeStats,
    ) {
        let mut needed: FastSet<(u64, nxkit_metal::TextureKind)> = FastSet::default();
        let stages = detail
            .draws
            .iter()
            .flat_map(|d| {
                [
                    (Some(&*d.program.vertex).filter(|k| !k.is_empty()), true, crate::detail::Stage::Vertex, &d.textures),
                    (d.program.fragment.as_deref(), false, crate::detail::Stage::Fragment, &d.textures),
                    (d.program.task.as_deref(), false, crate::detail::Stage::Task, &d.textures),
                    (d.program.mesh.as_deref(), false, crate::detail::Stage::Mesh, &d.textures),
                ]
            })
            .chain(
                detail
                    .dispatches
                    .iter()
                    .map(|d| (Some(d.program.as_str()), false, crate::detail::Stage::Compute, &d.textures)),
            );
        for (key, vertex, stage, binds) in stages {
            let Some(key) = key else { continue };
            if binds.is_empty() {
                continue;
            }
            for &(reg, k) in declared_views(&mut self.fit_decl, &self.programs, key, vertex) {
                let Some(h) = binds.iter().find(|t| t.stage == stage && t.slot == reg).map(|t| t.handle) else {
                    continue;
                };
                if h != 0 && handles.map.contains_key(&h) {
                    needed.insert((h, k));
                }
            }
        }
        for (h, k) in needed {
            let Ok((slot, _)) = view.resolve(h) else { continue };
            let Some(res) = textures.get(&slot.texture) else { continue };
            let Ok(base) = self.store.view(slot.texture, res, &slot.header, stats) else { continue };
            if base.desc().kind == k {
                continue;
            }
            if let Ok(t) = self.store.view_as(slot.texture, res, &slot.header, Some(k), stats) {
                static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if crate::diag("fit") && crate::diag::first(&SHOWN, 300) {
                    eprintln!("DIAG fit handle {h:#x} {:?} {:?} -> {k:?}", slot.texture, base.desc().kind);
                }
                handles.fits.insert((h, k), t.gpu_resource_id());
                handles.keep.push(t);
            }
        }
    }

    /// The `stale` category: hashes the guest bytes of a texture every frame and prints when they changed, with the
    /// residency that the front end has for it. A change while the texture is not `GuestNewer` is a CPU write that the
    /// front end did not see. It reads the whole texture, so `NXKIT_DIAG_TEX` should narrow it down.
    fn stale_diag(&self, id: TexId, rec: &nxkit_nvn::resource::TextureRecord, res: &Res) {
        use std::hash::{Hash, Hasher};
        static SEEN: Mutex<Option<FastMap<u64, u64>>> = Mutex::new(None);
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let mut buf = vec![0u8; rec.layout.size as usize];
        let off = rec.address.get().wrapping_sub(self.guest.address_map().gpu_base());
        if self.guest.read(off, &mut buf).is_err() {
            return;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        buf.hash(&mut h);
        let h = h.finish();
        let mut g = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
        let key = rec.address.get();
        if g.get_or_insert_with(FastMap::default).insert(key, h).is_some_and(|old| old != h)
            && crate::diag::first(&SHOWN, 200)
        {
            eprintln!(
                "DIAG stale {id:?} {:?} {}x{} addr {key:#x} changed, residency {:?} needs upload {}",
                res.desc.format, res.desc.width, res.desc.height, rec.residency, TextureStore::needs_upload(res, rec)
            );
        }
    }

    fn resolve_one(
        &mut self,
        view: &dyn EncodeView,
        handle: u64,
        scale: f32,
        textures: &mut FastMap<TexId, Res>,
        stats: &mut EncodeStats,
    ) -> std::result::Result<(Resolved, Texture), Skip> {
        let (slot, tsc) = view.resolve(handle).map_err(|e| {
            let what = format_args!("handle {handle:#x}: {e:?}");
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::HandleUnresolved, what);
            crate::tex_skip(what)
        })?;
        let id = slot.texture;
        if !textures.contains_key(&id) {
            let rec = view.record(id).map_err(|e| {
                let what = format_args!("handle {handle:#x} tex {id:?}: {e:?}");
                nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::HandleUnresolved, what);
                crate::tex_skip(what)
            })?;
            let res = self.store.ensure(rec, scale, Use::default(), stats)?;
            textures.insert(id, res);
        }
        let res = textures.get(&id).ok_or(Skip::Texture)?;
        let tex_view = self.store.view(id, res, &slot.header, stats)?;
        if crate::diag("barr") {
            crate::barr::note(handle, id);
        }
        if crate::diag("bc") && res.desc.format.is_compressed() {
            static SEEN_BC: std::sync::Mutex<Vec<TexId>> = std::sync::Mutex::new(Vec::new());
            if let (Ok(rec), Ok(mut seen)) = (view.record(id), SEEN_BC.lock()) {
                if !seen.contains(&id) && seen.len() < 200 {
                    seen.push(id);
                    let map = self.guest.address_map();
                    let mut head = vec![0u8; 32.min(rec.layout.size as usize)];
                    let read = rec
                        .address
                        .get()
                        .checked_sub(map.gpu_base())
                        .is_some_and(|off| self.guest.read(off, &mut head).is_ok());
                    eprintln!(
                        "DIAG bc {id:?} {}x{} {:?} residency {:?} size {} guest_read {read} head {head:02x?}",
                        res.desc.width, res.desc.height, res.desc.format, rec.residency, rec.layout.size
                    );
                }
            }
        }
        if crate::diag("hnd") {
            static SEEN: std::sync::Mutex<Vec<(u64, TexId, u64)>> = std::sync::Mutex::new(Vec::new());
            let key = (handle, id, 0u64);
            if let Ok(mut seen) = SEEN.lock() {
                if !seen.contains(&key) && seen.len() < 2000 {
                    seen.push(key);
                    let v = tex_view.desc();
                    eprintln!(
                        "DIAG hnd {handle} {id:?} backing {}x{} {:?} scaled {} view {}x{} {:?} kind {:?}",
                        res.desc.width, res.desc.height, res.desc.format, res.scaled, v.width, v.height, v.format, v.kind
                    );
                    if matches!(v.kind, nxkit_metal::TextureKind::Cube) {
                        eprintln!(
                            "DIAG hnd cube {handle} {id:?} swizzle {:?} view {:?} levels {}+{} backing_levels {} identity_view {}",
                            slot.header.view.swizzle, slot.header.view.format, slot.header.view.base_level, slot.header.view.levels,
                            res.desc.mip_levels, tex_view.gpu_resource_id() == res.tex.gpu_resource_id()
                        );
                    }
                }
            }
        }
        let sampler = self.bindless.sampler(tsc)?;
        Ok((
            Resolved {
                texture: tex_view.gpu_resource_id(),
                sampler,
            },
            tex_view,
        ))
    }

    /// Encodes, at the end of `cb` (the last command buffer of the frame), the copy-back of the written textures
    /// whose storage the CPU can access. The result is the textures copied back and their storage ranges.
    #[allow(clippy::too_many_arguments)]
    fn copy_back_written(
        &mut self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        view: &dyn EncodeView,
        textures: &FastMap<TexId, Res>,
        dirty: &FastMap<TexId, (Dirty, usize)>,
        copied_back: &[TexId],
        stats: &mut EncodeStats,
    ) -> (Vec<TexId>, Vec<(u64, u64)>) {
        // The last writer first: of two textures over the same storage, the one written last is copied back.
        let mut order: Vec<(usize, TexId)> = dirty
            .iter()
            .filter(|(_, (d, _))| !d.is_empty())
            .map(|(id, (_, last))| (*last, *id))
            .collect();
        order.sort_unstable_by(|a, b| b.cmp(a));
        let mut items = Vec::new();
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        let mut backings: FastSet<TexId> = FastSet::default();
        let (mut planned, mut over_budget) = (0u64, 0u32);
        for (_, id) in order {
            let (Some(res), Ok(rec)) = (textures.get(&id), view.record(id)) else { continue };
            if res.desc.storage == nxkit_metal::Storage::Memoryless || copied_back.contains(&id) {
                continue;
            }
            let Some(flags) = self.copyback.candidate(id, rec) else { continue };
            let (start, end) = (rec.address.get(), rec.end());
            if backings.contains(&res.barrier_id) || ranges.iter().any(|&(s, e)| s < end && start < e) {
                continue;
            }
            if planned + rec.layout.size > crate::copyback::budget() {
                over_budget += 1;
                nxkit_nvn::diag::note(
                    nxkit_nvn::diag::Reason::CopybackOverBudget,
                    format_args!("{id:?} {:?} {}x{} {} bytes", res.desc.format, res.desc.width, res.desc.height, rec.layout.size),
                );
                continue;
            }
            planned += rec.layout.size;
            backings.insert(res.barrier_id);
            ranges.push((start, end));
            items.push((id, rec, res, &dirty[&id].0, flags));
        }
        let args: Vec<_> = items.iter().map(|(_, rec, res, d, _)| (*rec, *res, *d)).collect();
        let results = self.transfer.copy_back_many(cb, scratch, &self.guest, &args);
        let (mut done, mut done_ranges) = (Vec::new(), Vec::new());
        let (mut bytes, mut refused) = (0u64, 0u32);
        for ((id, rec, res, d, flags), r) in items.iter().zip(results) {
            match r {
                Ok(n) => {
                    stats.copybacks += 1;
                    stats.copyback_bytes += n;
                    bytes += n;
                    done.push(*id);
                    done_ranges.push((rec.address.get(), rec.end()));
                    self.barriers
                        .note_read(res.barrier_id, Stages::DISPATCH | Stages::BLIT);
                    self.copyback.log_first(*id, rec, res, *flags, d, n);
                }
                Err(why) => {
                    refused += 1;
                    stats.skip(why);
                    let what = format_args!(
                        "{id:?} {:?} {}x{} layers {} levels {} scaled {} widened {} linear {} stride {}: {why:?}",
                        res.desc.format, res.desc.width, res.desc.height, res.desc.array_layers,
                        res.desc.mip_levels, res.scaled, res.widened, rec.layout.linear, rec.desc.stride
                    );
                    nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::CopybackRefused, what);
                    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    if crate::diag("copyback") && crate::diag::first(&N, 100) {
                        eprintln!("DIAG copyback refused {what}");
                    }
                }
            }
        }
        if !items.is_empty() {
            stats.compute_passes += 1;
        }
        self.copyback.log_frame(done.len() as u32, bytes, refused, over_budget);
        (done, done_ranges)
    }

    /// Encodes a held frame and submits it.
    ///
    /// `frame` and `plan` come from `nxkit-nvn` (`Frame` and `analyse`). `detail` carries what the commands of the
    /// frame bound, `view` gives the texture records and resolves texture handles. `scale` is the factor for textures that are rendered
    /// into (2.0 for 1080p to 4K, 1.0 for none). The call returns when the command buffers are committed. The GPU
    /// runs on. Call [`Gpu::wait`] before the CPU reads a texture that the report lists in `copied_back`.
    pub fn encode_frame(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        view: &dyn EncodeView,
        detail: &FrameDetail,
        scale: f32,
    ) -> Result<EncodeReport> {
        nxkit_metal::autorelease_pool(|| self.encode_frame_inner(frame, plan, view, detail, scale))
    }

    fn encode_frame_inner(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        view: &dyn EncodeView,
        detail: &FrameDetail,
        scale: f32,
    ) -> Result<EncodeReport> {
        crate::refresh_knobs();
        let diag_frame = crate::diag::begin_frame();
        let lum = crate::lum::begin();
        let t0 = Instant::now();
        MARKS.with(|m| m.borrow_mut().clear());
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let mut stats = EncodeStats::default();
        let builds_before = self.pipelines.counts().0;

        // The batch comes first, so that the uniform snapshots go to its scratch memory on a helper thread while this
        // one resolves the textures; the chunks need both.
        let ends = if plan.passes.is_empty() {
            Vec::new()
        } else {
            let weights: Vec<u32> = plan
                .passes
                .iter()
                .map(|p| p.cmds.len() as u32 + 1)
                .collect();
            jobs::split(&weights, self.threads * CHUNKS_PER_THREAD)
        };
        let t_ring = Instant::now();
        let mut batch = self.ring.begin(1 + ends.len())?;
        PHASES.with(|p| p.borrow_mut()[0] = nanos(t_ring, Instant::now()));
        let scratch = batch.scratch();
        let preload = {
            let snaps: Vec<Arc<[u8]>> = detail
                .draws
                .iter()
                .flat_map(|d| d.ubos.iter().chain(d.sbufs.iter()))
                .chain(detail.dispatches.iter().flat_map(|d| d.ubos.iter()))
                .filter_map(|u| u.snapshot.clone())
                .collect();
            let scratch = scratch.clone();
            std::thread::Builder::new().name("nxkit-preload".into()).spawn(move || {
                // Every uniform snapshot of the frame goes to the scratch memory once. The chunks then only look the
                // addresses up; when each chunk uploaded on first use, the shared map was locked twice per uniform of
                // every draw by all chunk threads.
                let mut preloaded: FastMap<usize, u64> = FastMap::default();
                for bytes in &snaps {
                    let key = bytes.as_ptr() as usize;
                    if !preloaded.contains_key(&key)
                        && let Ok(a) = scratch.push(bytes)
                    {
                        preloaded.insert(key, a);
                    }
                }
                preloaded
            })
        };
        mark(t0, "ring");
        let mut uses = collect_uses(frame, plan);
        self.copyback.note_presented(frame);
        // A render target that is copied back stays at the native size: the copy-back needs the guest layout. The
        // other attachments of its passes stay at the native size too, since a pass renders at one scale.
        if crate::copyback::enabled() {
            let mut pin: FastSet<TexId> = uses
                .iter()
                .filter(|(id, u)| {
                    u.attachment
                        && view
                            .record(**id)
                            .is_ok_and(|rec| self.copyback.candidate(**id, rec).is_some())
                })
                .map(|(id, _)| *id)
                .collect();
            if !pin.is_empty() {
                let mut co = Vec::new();
                for p in plan.passes.iter().filter(|p| p.kind == PassKind::Render) {
                    let atts = plan.attachments_of(p);
                    if atts.iter().any(|a| pin.contains(&a.tex)) {
                        co.extend(atts.iter().map(|a| a.tex).filter(|t| !pin.contains(t)));
                    }
                }
                for t in co {
                    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    if pin.insert(t)
                        && crate::diag("copyback")
                        && SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 50
                    {
                        eprintln!("DIAG copyback pins {t:?} at the native size, it shares a pass with a copied-back texture");
                    }
                }
                for id in pin {
                    if let Some(u) = uses.get_mut(&id) {
                        u.cpu_read = true;
                    }
                }
            }
        }
        for (id, u) in &uses {
            self.store.note(*id, *u);
        }
        let mut textures: FastMap<TexId, Res> = FastMap::default();
        for (id, u) in &uses {
            match view.record(*id) {
                Ok(rec) => match self.store.ensure(rec, scale, *u, &mut stats) {
                    Ok(r) => {
                        textures.insert(*id, r);
                    }
                    Err(why) => stats.skip(why),
                },
                Err(e) => stats.skip(crate::tex_skip(format_args!("use {id:?}: {e:?}"))),
            }
        }
        mark(t0, "uses+ensure");
        let t_resolve = Instant::now();
        let mut handles =
            self.resolve_handles(view, detail, scale, &mut textures, &mut stats);
        let t_fit = Instant::now();
        self.fit_handles(view, detail, &textures, &mut handles, &mut stats);
        stats.resolve_ns = nanos(t_resolve, t_fit);
        stats.fit_ns = nanos(t_fit, Instant::now());
        drop(self.store.take_retired());
        self.store.trim_views();

        mark(t0, "resolve+fit");
        let mut first_write: FastMap<TexId, usize> = FastMap::default();
        let mut gpu_wrote: Vec<TexId> = Vec::new();
        // The levels and layers that the frame wrote, and the last pass that wrote them, for the copy-back.
        let mut dirty: FastMap<TexId, (Dirty, usize)> = FastMap::default();
        for (i, p) in plan.passes.iter().enumerate() {
            if !matches!(
                p.kind,
                PassKind::Render | PassKind::Compute | PassKind::Copy
            ) {
                continue;
            }
            for id in touch(frame, plan, p).writes {
                let memoryless = textures
                    .get(&id)
                    .is_none_or(|r| r.desc.storage == nxkit_metal::Storage::Memoryless);
                if !memoryless && first_write.insert(id, i).is_none() {
                    gpu_wrote.push(id);
                }
                if !memoryless && crate::copyback::enabled() {
                    let e = dirty.entry(id).or_default();
                    e.1 = i;
                    if p.kind != PassKind::Render {
                        e.0 = Dirty::All;
                    }
                }
            }
            if p.kind == PassKind::Render {
                for a in plan.attachments_of(p) {
                    let Some((d, _)) = dirty.get_mut(&a.tex) else { continue };
                    let three_d = textures
                        .get(&a.tex)
                        .is_some_and(|r| r.desc.kind == nxkit_metal::TextureKind::D3);
                    if three_d {
                        *d = Dirty::All;
                    } else if a.store == nxkit_nvn::passes::StoreAction::Store {
                        d.add(a.view.level(), a.view.layer());
                    }
                }
            }
        }

        pass_shape_diag(frame, plan, &textures);
        mark(t0, "pre-ring");
        let keep: Arc<Keep> = {
            let (a, b) = self.bindless.pool_buffers();
            let mut all: Vec<Texture> = textures.values().map(|r| r.tex.clone()).collect();
            all.extend(handles.keep);
            Arc::new((all, a, b))
        };

        let mut deswizzled = Vec::new();
        let (first, rest) = batch
            .buffers()
            .split_first_mut()
            .ok_or(MetalError::Invalid("the batch has no buffers"))?;
        first.retain(keep.clone());
        for (id, res) in &textures {
            let Ok(rec) = view.record(*id) else { continue };
            if res.scaled && rec.residency == nxkit_nvn::resource::Residency::GuestNewer {
                stats.scaled_upload_lost += 1;
            }
            if crate::diag("stale") && crate::diag::tex_match(res.desc.format, res.desc.width, res.desc.height, res.desc.array_layers) {
                self.stale_diag(*id, rec, res);
            }
            if !TextureStore::needs_upload(res, rec) {
                continue;
            }
            let wait = self.copyback.upload_waits(rec);
            let t_up = Instant::now();
            let r = self.transfer.upload(first, &scratch, &self.guest, rec, res, wait);
            stats.upload_ns += nanos(t_up, Instant::now());
            static UPLOADS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if crate::diag("upload")
                && crate::diag::tex_filtered()
                && crate::diag::tex_match(res.desc.format, res.desc.width, res.desc.height, res.desc.array_layers)
                && crate::diag::first(&UPLOADS, 200)
            {
                eprintln!(
                    "DIAG upload {id:?} {:?} {}x{} residency {:?} scaled {} linear {}: {:?}",
                    res.desc.format, res.desc.width, res.desc.height, rec.residency, res.scaled, rec.layout.linear,
                    r.as_ref().err()
                );
            }
            match r {
                Ok(()) => {
                    stats.uploads += 1;
                    stats.compute_passes += 1;
                    deswizzled.push(*id);
                    self.barriers
                        .seed(res.barrier_id, Stages::DISPATCH | Stages::BLIT);
                }
                Err(why) => stats.skip(why),
            }
        }

        let aliases = textures
            .iter()
            .map(|(id, res)| (*id, res.barrier_id))
            .collect();
        mark(t0, "pre-ords");
        let ords = ordinals(frame);
        kpass_diag(frame, plan, detail, &ords, &textures);
        mark(t0, "ords+kpass");
        let t_barrier = Instant::now();
        let vertex_samples = vertex_sampling_passes(frame, plan, detail, &ords);
        let uses = crate::hazards::pass_uses(frame, plan, detail, &ords, &self.programs, view);
        let barrier_plan = self.barriers.plan(frame, plan, &aliases, &vertex_samples, &uses);
        stats.barrier_ns = nanos(t_barrier, Instant::now());
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            if crate::diag("barr") && N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 120 == 0 {
                for line in barrier_plan.describe() {
                    eprintln!("{line}");
                }
            }
        }
        mark(t0, "barriers");
        let run_at: FastMap<u32, RecordingRun> =
            detail.runs.iter().map(|r| (r.first_draw, *r)).collect();
        let preloaded: FastMap<usize, u64> = match preload {
            Ok(h) => h.join().unwrap_or_default(),
            Err(_) => FastMap::default(),
        };
        mark(t0, "preload");
        let uploaded = Mutex::new(FastMap::default());
        let sh = Shared {
            ctx: &self.ctx,
            frame,
            plan,
            detail,
            table: view,
            textures: &textures,
            texture_store: &self.store,
            programs: &self.programs,
            pipelines: &self.pipelines,
            env: Env {
                handles: &handles.map,
                fits: &handles.fits,
                pools: self.bindless.pools(),
                default_sampler: self.bindless.default_sampler_id(),
                statics: self.bindless.statics(),
            },
            guest: &self.guest,
            icbs: &self.icbs,
            ordinals: &ords,
            run_at: &run_at,
            barriers: &barrier_plan,
            transfer: &self.transfer,
            first_write: &first_write,
            scale,
            uploaded: &uploaded,
            preloaded: &preloaded,
        };
        let mut start = 0;
        let ranges: Vec<Range<usize>> = ends
            .iter()
            .map(|e| {
                let r = start..*e;
                start = *e;
                r
            })
            .collect();
        let items: Vec<(&mut CommandBuffer, Range<usize>)> = rest.iter_mut().zip(ranges).collect();
        mark(t0, "pre-chunks");
        let t_chunks = Instant::now();
        let results = jobs::run_all(self.threads, items, |(cb, range)| {
            let (start, cpu0) = (Instant::now(), nxkit_metal::thread::cpu_time_ns());
            let passes = range.len();
            let r = run_chunk(&sh, cb, &scratch, &keep, range);
            let timing = ChunkTiming {
                delay_ns: nanos(t_chunks, start),
                wall_ns: nanos(start, Instant::now()),
                cpu_ns: nxkit_metal::thread::cpu_time_ns().saturating_sub(cpu0),
                passes,
            };
            (r, timing)
        });
        let chunks_ns = nanos(t_chunks, Instant::now());
        let mut copied_back = Vec::new();
        let mut timings = Vec::with_capacity(results.len());
        for ((s, copied), timing) in results {
            stats.add(&s);
            copied_back.extend(copied);
            timings.push(timing);
        }
        CHUNKS.with(|c| *c.borrow_mut() = timings);
        let (mirrored, mirrored_ranges) = match rest.last_mut() {
            Some(cb) if !dirty.is_empty() => {
                {
                    let t_cb = Instant::now();
                    let r = self.copy_back_written(cb, &scratch, view, &textures, &dirty, &copied_back, &mut stats);
                    PHASES.with(|p| p.borrow_mut()[1] = nanos(t_cb, Instant::now()));
                    r
                }
            }
            _ => Default::default(),
        };

        stats.chunks_ns = chunks_ns;
        stats.barriers_emitted = barrier_plan.emitted;
        stats.barriers_avoided = barrier_plan.avoided;
        stats.command_buffers = 1 + ends.len() as u32;
        stats.pipeline_builds = self.pipelines.counts().0.saturating_sub(builds_before) as u32;
        mark(t0, "chunk-end");
        stats.encode_ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        hitch_diag(&stats, detail.draws.len());

        // ICBs and argument buffers made during the encode join the residency set before the commit.
        let t_commit = Instant::now();
        self.ctx.commit_residency();
        let t_submit = Instant::now();
        let submitted = batch.submit()?;
        PHASES.with(|p| {
            let mut p = p.borrow_mut();
            p[2] = nanos(t_commit, t_submit);
            p[3] = nanos(t_submit, Instant::now());
        });
        let number = submitted.number();
        self.older = self.last.replace(submitted);
        self.copyback.submitted(number, &mirrored_ranges);
        if lum {
            let _ = self.wait();
            crate::lum::dump(&self.ctx);
        }
        crate::health::frame(diag_frame, &stats);
        Ok(EncodeReport {
            batch: number,
            deswizzled,
            gpu_wrote,
            copied_back,
            mirrored,
            stats,
        })
    }
}

/// `NXKIT_DIAG=passes`: every 300 frames, the render passes of the frame by the number of draws and by the size of
/// their first attachment, to see which passes make the encoder boundaries of the GPU.
fn pass_shape_diag(
    frame: &nxkit_nvn::frame::Frame,
    plan: &nxkit_nvn::passes::PassPlan,
    textures: &crate::draws::FastMap<nxkit_nvn::resource::TexId, Res>,
) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    if !crate::diag("passes") || N.fetch_add(1, Ordering::Relaxed) % 300 != 150 {
        return;
    }
    let mut by_draws = std::collections::BTreeMap::<&str, u32>::new();
    let mut by_size = std::collections::BTreeMap::<String, (u32, u32)>::new();
    for p in plan.passes.iter().filter(|p| p.kind == nxkit_nvn::passes::PassKind::Render) {
        let cmds = &frame.cmds[p.cmds.start as usize..p.cmds.end as usize];
        let draws = cmds.iter().filter(|c| matches!(c, nxkit_nvn::frame::Cmd::Draw { .. })).count();
        let bucket = match draws {
            0 => "0 (clears)",
            1 => "1",
            2 => "2",
            3..=10 => "3-10",
            11..=100 => "11-100",
            _ => ">100",
        };
        *by_draws.entry(bucket).or_default() += 1;
        let size = plan
            .attachments_of(p)
            .first()
            .and_then(|a| textures.get(&a.tex))
            .map_or_else(|| "?".to_string(), |r| format!("{}x{} {:?}", r.desc.width, r.desc.height, r.desc.format));
        let e = by_size.entry(size).or_default();
        e.0 += 1;
        e.1 += draws as u32;
    }
    eprintln!("DIAG passes by draws: {by_draws:?}");
    if std::env::var_os("NXKIT_PASS_LIST").is_some() {
        for (i, p) in plan.passes.iter().enumerate() {
            let cmds = &frame.cmds[p.cmds.start as usize..p.cmds.end as usize];
            let draws = cmds.iter().filter(|c| matches!(c, nxkit_nvn::frame::Cmd::Draw { .. })).count();
            let atts: Vec<String> = plan
                .attachments_of(p)
                .iter()
                .filter_map(|a| textures.get(&a.tex).map(|r| format!("{}x{} {:?} {:?}/{:?}", r.desc.width, r.desc.height, r.desc.format, a.load, a.store)))
                .collect();
            eprintln!("DIAG pass {i:3} {:?} {:?} draws {draws} cmds {} {atts:?}", p.kind, p.begin, cmds.len());
        }
    }
    let mut v: Vec<_> = by_size.into_iter().collect();
    v.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (size, (n, draws)) in v.iter().take(20) {
        eprintln!("DIAG passes {n:4} with {draws:5} draws into {size}");
    }
}

/// `NXKIT_DIAG=hitch`: a line for each frame whose encode took long or that built pipelines or textures, with the wall
/// clock in milliseconds, to put it next to the late frames of `NXKIT_PACE_DIAG`.
fn hitch_diag(s: &EncodeStats, draws: usize) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| crate::diag("hitch")) {
        return;
    }
    if s.encode_ns < 7_000_000 && s.pipeline_builds == 0 && s.textures_created == 0 {
        return;
    }
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() % 1_000_000);
    let phases = PHASES.with(|p| *p.borrow());
    let chunks = CHUNKS.with(|c| {
        c.borrow()
            .iter()
            .map(|t| {
                format!(
                    "{}p +{:.1} {:.1}/{:.1}",
                    t.passes,
                    t.delay_ns as f64 / 1e6,
                    t.wall_ns as f64 / 1e6,
                    t.cpu_ns as f64 / 1e6
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    });
    let marks = MARKS.with(|m| {
        let m = m.borrow();
        let mut last = 0;
        m.iter()
            .map(|(n, at)| {
                let d = at - last;
                last = *at;
                format!("{n} {:.2}", d as f64 / 1e6)
            })
            .collect::<Vec<_>>()
            .join(", ")
    });
    eprintln!("DIAG hitch steps: {marks}");
    if s.chunks_ns > 5_000_000 {
        eprintln!("DIAG hitch chunks (passes +start wall/cpu ms): {chunks}");
    }
    eprintln!(
        "DIAG hitch t {ms} encode {:.2} ms chunks {:.2} ms create {:.2} ms upload {:.2} ms resolve {:.2} ms ring {:.2} ms copyback {:.2} ms (last commit {:.2} submit {:.2}) pipelines {} textures {} uploads {} copybacks {} passes {}+{} draws {draws}",
        s.encode_ns as f64 / 1e6,
        s.chunks_ns as f64 / 1e6,
        s.texture_create_ns as f64 / 1e6,
        s.upload_ns as f64 / 1e6,
        s.resolve_ns as f64 / 1e6,
        phases[0] as f64 / 1e6,
        phases[1] as f64 / 1e6,
        phases[2] as f64 / 1e6,
        phases[3] as f64 / 1e6,
        s.pipeline_builds,
        s.textures_created,
        s.uploads,
        s.copybacks,
        s.render_passes,
        s.compute_passes,
    );
}

/// Notes the time since `t0` under `name`, for the `hitch` line.
fn mark(t0: Instant, name: &'static str) {
    MARKS.with(|m| m.borrow_mut().push((name, nanos(t0, Instant::now()))));
}

thread_local! {
    /// The steps of the last encode on this thread and the time since its start, for `NXKIT_DIAG=hitch`.
    static MARKS: std::cell::RefCell<Vec<(&'static str, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The time of the phases of the last encode on this thread, for `NXKIT_DIAG=hitch`: the wait for a free batch, the
    /// copy-back, the residency commit and the submit.
    static PHASES: std::cell::RefCell<[u64; 4]> = const { std::cell::RefCell::new([0; 4]) };
}

/// When a chunk of the encode started, how long it ran on the wall clock and on its thread's CPU clock.
#[derive(Clone, Copy, Default)]
struct ChunkTiming {
    delay_ns: u64,
    wall_ns: u64,
    cpu_ns: u64,
    passes: usize,
}

thread_local! {
    /// The chunks of the last encode on this thread, for `NXKIT_DIAG=hitch`.
    static CHUNKS: std::cell::RefCell<Vec<ChunkTiming>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// For each pass, whether one of its draws binds a texture in the vertex stage. Only such a render pass has to hold its
/// vertex stage until an earlier pass wrote what it samples; the others let their vertex work overlap the fragment work
/// before them.
fn vertex_sampling_passes(
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[crate::draws::Ordinal],
) -> Vec<bool> {
    plan.passes
        .iter()
        .map(|p| {
            if p.kind != nxkit_nvn::passes::PassKind::Render {
                return false;
            }
            let first = ords.get(p.cmds.start as usize).map_or(0, |o| o.draw) as usize;
            let n = p
                .cmds
                .clone()
                .filter(|i| matches!(frame.cmds.get(*i as usize), Some(nxkit_nvn::frame::Cmd::Draw { .. })))
                .count();
            detail.draws.get(first..first + n).is_none_or(|draws| {
                draws.iter().any(|d| d.textures.iter().any(|t| t.stage == crate::detail::Stage::Vertex))
            })
        })
        .collect()
}

/// `NXKIT_DIAG=kpass` (`docs/plans/kernel-passes.md`, step 0): every 300 frames, the render passes that could run as a
/// compute dispatch instead (one draw of at most 6 vertices or indices, one instance, no depth, stencil, blending or
/// alpha to coverage, one sample, colour targets that a kernel can write, at most 512x512), the runs of them in a row,
/// and why the other one-draw passes cannot.
fn kpass_diag(
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[crate::draws::Ordinal],
    textures: &FastMap<TexId, Res>,
) {
    use nxkit_metal::Format as F;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    if !crate::diag("kpass") || N.fetch_add(1, Ordering::Relaxed) % 300 != 150 {
        return;
    }
    const MAX_PX: u64 = 512 * 512;
    let mut why = std::collections::BTreeMap::<&str, u32>::new();
    let (mut candidates, mut renders, mut runs, mut in_run) = (0u32, 0u32, 0u32, false);
    let mut sizes = std::collections::BTreeMap::<String, u32>::new();
    for p in &plan.passes {
        let kind_render = p.kind == nxkit_nvn::passes::PassKind::Render;
        let mut ok = false;
        if kind_render {
            renders += 1;
            let cmds = &frame.cmds[p.cmds.start as usize..p.cmds.end as usize];
            let draws = cmds.iter().filter(|c| matches!(c, Cmd::Draw { .. })).count();
            let reason = if draws != 1 {
                Some(if draws == 0 { "no draw" } else { "several draws" })
            } else {
                let first = ords.get(p.cmds.start as usize).map_or(0, |o| o.draw) as usize;
                let atts = plan.attachments_of(p);
                let colors: Vec<&Res> = atts
                    .iter()
                    .filter(|a| matches!(a.slot, nxkit_nvn::passes::Slot::Color(_)))
                    .filter_map(|a| textures.get(&a.tex))
                    .collect();
                match detail.draws.get(first) {
                    None => Some("no detail"),
                    Some(d) => {
                        let small_geometry = match d.args {
                            DrawArgs::Arrays { count, instances, .. } => count <= 6 && instances <= 1,
                            DrawArgs::Indexed { count, instances, .. } => count <= 6 && instances <= 1,
                            _ => false,
                        };
                        let ds = &d.depth_stencil;
                        if !small_geometry {
                            Some("more than 6 vertices or instanced")
                        } else if ds.depth_test || ds.depth_write || ds.stencil_test {
                            Some("depth or stencil")
                        } else if d.blend.iter().any(|b| b.enable) {
                            Some("blending")
                        } else if d.raster.alpha_to_coverage {
                            Some("alpha to coverage")
                        } else if colors.is_empty() {
                            Some("no colour target")
                        } else if colors.iter().any(|r| {
                            r.desc.format.is_compressed()
                                || r.desc.format.is_depth()
                                || matches!(r.desc.format, F::Rgba8UnormSrgb | F::Bgra8UnormSrgb)
                        }) {
                            Some("format")
                        } else if colors.iter().any(|r| u64::from(r.desc.width) * u64::from(r.desc.height) > MAX_PX) {
                            Some("larger than 512x512")
                        } else {
                            if let Some(r) = colors.first() {
                                *sizes.entry(format!("{}x{}", r.desc.width, r.desc.height)).or_default() += 1;
                            }
                            None
                        }
                    }
                }
            };
            match reason {
                None => ok = true,
                Some(r) if draws == 1 => *why.entry(r).or_default() += 1,
                Some(r) => *why.entry(r).or_default() += 1,
            }
        }
        if ok {
            candidates += 1;
            if !in_run {
                runs += 1;
            }
            in_run = true;
        } else if kind_render {
            in_run = false;
        }
    }
    eprintln!(
        "DIAG kpass: {candidates} of {renders} render passes are candidates, in {runs} runs; the others: {why:?}; candidate targets: {sizes:?}"
    );
}
