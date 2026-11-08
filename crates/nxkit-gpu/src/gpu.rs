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
    ArchiveConfig, BatchRing, CommandBuffer, CommandBufferStats, Context, DataBuffer, GobOrder, GuestMemory, Metrics,
    MetalError, Result, Scratch, Stages, SubmittedBatch, Texture,
};
use nxkit_nvn::descriptors::Descriptors;
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{PassKind, PassPlan};
use nxkit_nvn::resource::{ResourceTable, TexId};

use crate::barriers::{BarrierState, touch};
use crate::bindless::{Bindless, Env, Handles, Resolved};
use crate::detail::{FrameDetail, RecordingRun};
use crate::draws::{Shared, ordinals};
use crate::encode::{Tables, render_pass};
use crate::icbs::Icbs;
use crate::jobs;
use crate::passes::{compute_pass, cpu_read};
use crate::pipelines::Pipelines;
use crate::programs::Programs;
use crate::stats::{EncodeReport, EncodeStats, Skip};
use crate::textures::{Res, TextureStore, Use};
use crate::upload::Transfer;

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
            threads: std::thread::available_parallelism().map_or(2, |n| n.get()).min(6),
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
    pools: Vec<(u64, u64)>,
}

/// How each texture of the frame is used. A texture is memoryless only when every attachment use says so and no
/// command samples, copies, presents or reads it.
fn collect_uses(frame: &Frame, plan: &PassPlan) -> HashMap<TexId, Use> {
    let mut other: HashSet<TexId> = HashSet::new();
    let mut read_back: HashSet<TexId> = HashSet::new();
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
        let Some(pass) = sh.plan.passes.get(index) else { continue };
        match pass.kind {
            PassKind::Render => render_pass(sh, cb, &tables, scratch, index, pass, &mut stats),
            PassKind::Compute | PassKind::Copy => compute_pass(sh, cb, &tables, scratch, index, pass, &mut stats),
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
            ring: BatchRing::new(ctx, threads + 1)?,
            threads,
            last: None,
            pools: Vec::new(),
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
        self.icbs.lock().unwrap_or_else(PoisonError::into_inner).set_candidates(handles);
    }

    /// The cache dropped the summary of a recording, so its ICB is built again at the next use.
    pub fn recording_dropped(&self, handle: u64) {
        self.icbs.lock().unwrap_or_else(PoisonError::into_inner).drop_recording(handle);
    }

    /// The number of ICBs that are alive.
    pub fn icb_count(&self) -> usize {
        self.icbs.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Forgets the programs and the ICBs, for example after the shader directory changed. The pipelines stay.
    pub fn reload_shaders(&self) {
        self.programs.clear();
        self.icbs.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// The counters of the command buffers: CPU encode time and GPU time (R16).
    pub fn metrics(&self) -> Arc<Metrics> {
        self.ring.metrics()
    }

    /// The Metal texture that holds a texture of the game, as the last frame left it. None when no frame used it.
    pub fn texture(&self, id: TexId) -> Option<Texture> {
        self.store.get(id).map(|r| r.tex.clone())
    }

    /// Waits until the GPU finished the last submitted frame. The result is `None` when nothing was submitted.
    pub fn wait(&mut self) -> Result<Option<CommandBufferStats>> {
        self.last.take().map(|b| b.wait()).transpose()
    }

    fn resolve_handles(
        &mut self,
        table: &ResourceTable,
        descriptors: &Descriptors,
        detail: &FrameDetail,
        scale: f32,
        textures: &mut HashMap<TexId, Res>,
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
        for handle in wanted {
            if out.map.contains_key(&handle) {
                continue;
            }
            match self.resolve_one(table, descriptors, handle, scale, textures, stats) {
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
                    (Some(&*d.program.vertex), true, crate::detail::Stage::Vertex, &d.textures),
                    (d.program.fragment.as_deref(), false, crate::detail::Stage::Fragment, &d.textures),
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
                if crate::diag("fit") {
                    eprintln!("DIAG fit handle {h:#x} {:?} {:?} -> {k:?}", slot.texture, base.desc().kind);
                }
                handles.fits.insert((h, k), t.gpu_resource_id());
                handles.keep.push(t);
            }
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
        let (slot, tsc) = view
            .resolve(handle)
            .map_err(|e| crate::tex_skip(format_args!("handle {handle:#x}: {e:?}")))?;
        let id = slot.texture;
        if !textures.contains_key(&id) {
            let rec = view.record(id).map_err(|e| {
                crate::tex_skip(format_args!("handle {handle:#x} tex {id:?}: {e:?}"))
            })?;
            let res = self.store.ensure(rec, scale, Use::default(), stats)?;
            textures.insert(id, res);
        }
        let res = textures.get(&id).ok_or(Skip::Texture)?;
        let view = self.store.view(id, res, &slot.header, stats)?;
        let sampler = self.bindless.sampler(tsc)?;
        Ok((Resolved { texture: view.gpu_resource_id(), sampler }, view))
    }

    /// Encodes a held frame and submits it.
    ///
    /// `frame` and `plan` come from `nxkit-nvn` (`Frame` and `analyse`). `detail` carries what the commands of the
    /// frame bound, `descriptors` resolves texture handles. `scale` is the factor for textures that are rendered
    /// into (2.0 for 1080p to 4K, 1.0 for none). The call returns when the command buffers are committed. The GPU
    /// runs on. Call [`Gpu::wait`] before the CPU reads a texture that the report lists in `copied_back`.
    pub fn encode_frame(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        table: &ResourceTable,
        descriptors: &Descriptors,
        detail: &FrameDetail,
        scale: f32,
    ) -> Result<EncodeReport> {
        crate::refresh_knobs();
        let lum = crate::lum::begin();
        let t0 = Instant::now();
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let mut stats = EncodeStats::default();
        let builds_before = self.pipelines.counts().0;

        let uses = collect_uses(frame, plan);
        for (id, u) in &uses {
            self.store.note(*id, *u);
        }
        let mut textures: HashMap<TexId, Res> = HashMap::new();
        for (id, u) in &uses {
            match table.get(*id) {
                Ok(rec) => match self.store.ensure(rec, scale, *u, &mut stats) {
                    Ok(r) => {
                        textures.insert(*id, r);
                    }
                    Err(why) => stats.skip(why),
                },
                Err(e) => stats.skip(crate::tex_skip(format_args!("use {id:?}: {e:?}"))),
            }
        }
        let t_resolve = Instant::now();
        let mut handles =
            self.resolve_handles(view, detail, scale, &mut textures, &mut stats);
        let t_fit = Instant::now();
        self.fit_handles(view, detail, &textures, &mut handles, &mut stats);
        stats.resolve_ns = nanos(t_resolve, t_fit);
        stats.fit_ns = nanos(t_fit, Instant::now());
        drop(self.store.take_retired());
        self.store.trim_views();

        let mut first_write: HashMap<TexId, usize> = HashMap::new();
        let mut gpu_wrote: Vec<TexId> = Vec::new();
        for (i, p) in plan.passes.iter().enumerate() {
            if !matches!(p.kind, PassKind::Render | PassKind::Compute | PassKind::Copy) {
                continue;
            }
            for id in touch(frame, plan, p).writes {
                let memoryless = textures.get(&id).is_none_or(|r| r.desc.storage == nxkit_metal::Storage::Memoryless);
                if !memoryless && first_write.insert(id, i).is_none() {
                    gpu_wrote.push(id);
                }
            }
        }

        let ends = if plan.passes.is_empty() {
            Vec::new()
        } else {
            let weights: Vec<u32> = plan.passes.iter().map(|p| p.cmds.len() as u32 + 1).collect();
            jobs::split(&weights, self.threads)
        };
        let mut batch = self.ring.begin(1 + ends.len())?;
        let scratch = batch.scratch();
        let keep: Arc<Keep> = {
            let (a, b) = self.bindless.pool_buffers();
            let mut all: Vec<Texture> = textures.values().map(|r| r.tex.clone()).collect();
            all.extend(handles.keep);
            Arc::new((all, a, b))
        };

        let mut deswizzled = Vec::new();
        let (first, rest) = batch.buffers().split_first_mut().ok_or(MetalError::Invalid("the batch has no buffers"))?;
        first.retain(keep.clone());
        for (id, res) in &textures {
            let Ok(rec) = table.get(*id) else { continue };
            if res.scaled && rec.residency == nxkit_nvn::resource::Residency::GuestNewer {
                stats.scaled_upload_lost += 1;
            }
            let tile = crate::diag("hmcap")
                && res.desc.kind == nxkit_metal::TextureKind::D2
                && res.desc.format == nxkit_metal::Format::R16Unorm
                && res.desc.width == 256;
            if crate::diag("linstale") && rec.layout.linear {
                use std::hash::{Hash, Hasher};
                static SEEN: std::sync::Mutex<Option<FastMap<u64, u64>>> = std::sync::Mutex::new(None);
                let mut buf = vec![0u8; rec.layout.size as usize];
                let off = rec.address.get().wrapping_sub(self.guest.address_map().gpu_base());
                if self.guest.read(off, &mut buf).is_ok() {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    buf.hash(&mut h);
                    let h = h.finish();
                    let mut g = SEEN.lock().unwrap();
                    let m = g.get_or_insert_with(FastMap::default);
                    let key = rec.address.get();
                    if let Some(old) = m.insert(key, h) {
                        if old != h {
                            eprintln!("DIAG hmstale {id:?} {:?} {}x{} addr {key:#x} changed, residency {:?} needs {}", res.desc.format, res.desc.width, res.desc.height, rec.residency, TextureStore::needs_upload(res, rec));
                        }
                    }
                }
            }
            if tile {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 40 {
                    eprintln!(
                        "DIAG hmup {id:?} res {:?} scaled {} widened {} fresh {} storage {:?} needs {}",
                        rec.residency, res.scaled, res.widened, res.fresh, res.desc.storage,
                        TextureStore::needs_upload(res, rec)
                    );
                }
            }
            if !TextureStore::needs_upload(res, rec) {
                continue;
            }
            match self.transfer.upload(first, &scratch, &self.guest, rec, res) {
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
        if crate::diag("prov") && crate::lum::active() {
            let keys: Vec<String> = std::fs::read_to_string("/tmp/fmap/terrain_keys.txt")
                .map(|t| t.lines().map(str::to_owned).collect())
                .unwrap_or_default();
            let mut start: Vec<TexId> = Vec::new();
            for d in &detail.draws {
                if !keys.iter().any(|k| d.program.vertex.starts_with(k.as_str())) {
                    continue;
                }
                for t in d.textures.iter().filter(|t| t.handle != 0 && t.stage == crate::detail::Stage::Vertex && t.slot == 0) {
                    if let Ok((slot, _)) = view.resolve(t.handle) {
                        if !start.contains(&slot.texture) {
                            start.push(slot.texture);
                        }
                    }
                }
            }
            let touches: Vec<_> = plan.passes.iter().map(|p| crate::barriers::touch(frame, plan, p)).collect();
            let mut frontier = start.clone();
            let mut seen: Vec<TexId> = Vec::new();
            for depth in 0..4 {
                let mut next = Vec::new();
                for id in frontier.drain(..) {
                    if seen.contains(&id) {
                        continue;
                    }
                    seen.push(id);
                    let desc = textures.get(&id).map(|r| (r.desc.kind, r.desc.width, r.desc.height, r.desc.array_layers, r.desc.format));
                    let res = view.record(id).ok().map(|r| (r.residency, r.address.get()));
                    let writers: Vec<usize> = touches.iter().enumerate().filter(|(_, t)| t.writes.contains(&id)).map(|(i, _)| i).collect();
                    eprintln!("DIAG prov depth {depth} {id:?} {desc:?} {res:?} writers {writers:?}");
                    for w in writers.iter().take(6) {
                        let p = &plan.passes[*w];
                        let reads: Vec<_> = touches[*w].sampled.iter().map(|r| (*r, textures.get(r).map(|x| (x.desc.width, x.desc.format)))).collect();
                        eprintln!("DIAG prov   pass {w} {:?} cmds {:?} reads {reads:?}", p.kind, p.cmds);
                        next.extend(touches[*w].sampled.iter().copied());
                    }
                }
                frontier = next;
            }
        }
        if crate::diag("hm") {
            static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let is_hm = |id: &TexId| {
                textures.get(id).is_some_and(|r| {
                    r.desc.kind == nxkit_metal::TextureKind::D2Array && r.desc.array_layers == 8 && r.desc.width >= 256
                })
            };
            for (i, p) in plan.passes.iter().enumerate() {
                let t = crate::barriers::touch(frame, plan, p);
                let hit: Vec<_> = t.writes.iter().filter(|id| is_hm(id)).collect();
                if !hit.is_empty() && SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 300 {
                    let fmt: Vec<_> = hit.iter().map(|id| (**id, textures.get(id).map(|r| (r.desc.width, r.desc.format)))).collect();
                    let atts: Vec<_> = plan.attachments_of(p).iter().map(|a| (a.tex, a.slot, a.view.level(), a.view.layer(), a.load, a.store)).collect();
                    eprintln!("DIAG hm pass {i} {:?} cmds {:?} writes {fmt:?} atts {atts:?}", p.kind, p.cmds);
                }
            }
        }
        let t_barrier = Instant::now();
        let barrier_plan = self.barriers.plan(frame, plan, &aliases);
        stats.barrier_ns = nanos(t_barrier, Instant::now());
        let ords = ordinals(frame);
        let run_at: FastMap<u32, RecordingRun> =
            detail.runs.iter().map(|r| (r.first_draw, *r)).collect();
        let uploaded = Mutex::new(FastMap::default());
        let sh = Shared {
            ctx: &self.ctx,
            frame,
            plan,
            detail,
            table,
            textures: &textures,
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
        let t_chunks = Instant::now();
        let results = jobs::run_all(self.threads, items, |(cb, range)| {
            run_chunk(&sh, cb, &scratch, &keep, range)
        });
        let chunks_ns = nanos(t_chunks, Instant::now());
        let mut copied_back = Vec::new();
        for (s, copied) in results {
            stats.add(&s);
            copied_back.extend(copied);
        }

        stats.barriers_emitted = barrier_plan.emitted;
        stats.barriers_avoided = barrier_plan.avoided;
        stats.command_buffers = 1 + ends.len() as u32;
        stats.pipeline_builds = self.pipelines.counts().0.saturating_sub(builds_before) as u32;
        stats.encode_ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);

        // ICBs and argument buffers made during the encode join the residency set before the commit.
        self.ctx.commit_residency();
        let submitted = batch.submit()?;
        let number = submitted.number();
        self.last = Some(submitted);
        Ok(EncodeReport { batch: number, deswizzled, gpu_wrote, copied_back, stats })
    }
}
