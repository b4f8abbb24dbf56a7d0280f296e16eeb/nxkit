//! Bindless textures and samplers.
//!
//! A translated shader reads its textures and samplers from one argument buffer per stage (the host contract of D20).
//! A member of that buffer holds a resource ID. The game binds a texture handle (`texture_id | sampler_id << 20`).
//! The encoder resolves the handle to the Metal texture and sampler and writes their resource IDs into the argument
//! buffer, so a draw needs no bind call per texture. Shaders that index a pool read two host tables, one of texture
//! resource IDs by descriptor id and one of sampler resource IDs by sampler id.

use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use nxkit_metal::{Context, DataBuffer, Sampler, Scratch, Texture};
use nxkit_nvn::tic::Tsc;

use crate::detail::{Stage, TextureBind, UboDetail};
use nxkit_nvn::diag::{Reason, note_n};
use crate::programs::{BindKind, StageProgram};
use crate::stats::Skip;
use crate::translate;

/// The bytes before the first element of a pool table. The layout of the tables is unverified against the shader
/// step. The check is to compile a shader with a pool binding and read the index expression in its Metal source.
pub(crate) const POOL_HEAD: u64 = 16;
/// The bytes of one pool element: a resource ID.
pub(crate) const POOL_ELEMENT: u64 = 8;
/// The size of the driver constant block.
pub(crate) const DRV_SIZE: usize = 0xA00;
const DRV_C2_TEX: usize = 0x20;
const DRV_C2_TEX_SLOTS: usize = 41;
const DRV_DISPATCH: usize = 0x200;
const DRV_CS_UBO: usize = 0x210;
const DRV_CS_UBO_SLOTS: usize = 16;
const DRV_BASE_INSTANCE: usize = 0x644;

/// The sampler of a slot that has no sampler header: linear filtering, clamp to edge.
const DEFAULT_TSC: Tsc = Tsc {
    min_filter: 1,
    mag_filter: 1,
    wrap_s: 0,
    wrap_t: 0,
    wrap_r: 0,
    compare_mode: 0,
    compare_func: 0,
    reduction: 0,
    lod_min: 0.0,
    lod_max: 1000.0,
    lod_bias: 0.0,
    aniso: 1.0,
    border: [0.0; 4],
};

/// The resource IDs of one texture handle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// The resource ID of the texture view, or 0.
    pub(crate) texture: u64,
    /// The resource ID of the sampler.
    pub(crate) sampler: u64,
    /// The texture is stored at the render scale, not at the size the game made it.
    pub(crate) scaled: bool,
}

/// The handles that a frame resolved, and the Metal objects behind them. The command buffers keep the textures alive.
#[derive(Default)]
pub(crate) struct Handles {
    pub(crate) map: crate::draws::FastMap<u64, Resolved>,
    pub(crate) keep: Vec<Texture>,
    /// Views of a handle in the texture type that a shader declares, where it differs from the descriptor's.
    pub(crate) fits: crate::draws::FastMap<(u64, nxkit_metal::TextureKind), u64>,
    /// The storage image views the shaders of the frame need.
    pub(crate) images: crate::draws::FastMap<ImageKey, ImageRes>,
}

/// The resource ID of a storage image view, and whether its texture is stored at the render scale. An `id` of 0 means
/// that the image cannot be bound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImageRes {
    /// The resource ID of the view.
    pub(crate) id: u64,
    /// The texture is stored at the render scale.
    pub(crate) scaled: bool,
}

/// The key of an image view: the handle, the texture type the shader declares and `ImgDecl::atomic_code`.
pub(crate) type ImageKey = (u64, nxkit_metal::TextureKind, u8);

/// A place that takes bytes and returns the GPU address of the copy.
pub(crate) trait Sink {
    fn put(&mut self, data: &[u8]) -> Result<u64, Skip>;

    /// Puts bytes that many draws share. A sink may put them once and return the same address again.
    fn put_shared(&mut self, data: &Arc<[u8]>) -> Result<u64, Skip> {
        self.put(data)
    }

    /// The memo of the argument buffers that this sink's draws share, when it keeps one.
    fn memo(&mut self) -> Option<&mut ArgsMemo> {
        None
    }
}

/// The argument buffers that the previous draws of one render pass wrote. A draw that binds what the draw before it
/// bound per stage reuses that buffer instead of writing a new one. The addresses point into the scratch buffer of
/// the batch, so a memo must not outlive the pass.
#[derive(Default)]
pub(crate) struct ArgsMemo {
    stages: [StageMemo; 2],
}

/// The last argument buffer of one stage, with the words that describe what it was built from.
#[derive(Default)]
struct StageMemo {
    last: Vec<u64>,
    next: Vec<u64>,
    addr: u64,
    valid: bool,
}

impl ArgsMemo {
    /// The address of the argument buffer for `inp`, when the previous draw of the stage built it from the same
    /// inputs. A miss prepares [`ArgsMemo::store`].
    fn lookup(&mut self, p: &StageProgram, inp: &StageInputs<'_>, pass_scale: f32) -> Option<u64> {
        let m = match inp.stage {
            Stage::Vertex => &mut self.stages[0],
            Stage::Fragment => &mut self.stages[1],
            Stage::Compute | Stage::Task | Stage::Mesh => return None,
        };
        m.next.clear();
        m.next.push(p as *const StageProgram as u64);
        m.next.push(u64::from(inp.base_instance));
        m.next.push(u64::from(pass_scale.to_bits()));
        // The push constants are shared until the game pushes again, so their address names their content.
        m.next.push(inp.push.map_or(0, |b| b.as_ptr() as u64));
        let mark = m.next.len();
        m.next.push(0);
        for u in inp.ubos.iter().filter(|u| u.stage == inp.stage) {
            let snapshot = u.snapshot.as_ref().map_or(0, |b| b.as_ptr() as u64);
            m.next.extend([u64::from(u.slot), snapshot, u.address, u64::from(u.size)]);
        }
        let ubos = m.next.len() - mark - 1;
        m.next[mark] = ubos as u64;
        for s in inp.sbufs.iter().filter(|s| s.stage == inp.stage) {
            let snapshot = s.snapshot.as_ref().map_or(0, |b| b.as_ptr() as u64);
            m.next.extend([u64::from(s.slot), s.address, snapshot]);
        }
        m.next.push(u64::MAX);
        for t in inp.textures.iter().filter(|t| t.stage == inp.stage) {
            m.next.extend([u64::from(t.slot), t.handle]);
        }
        m.next.push(u64::MAX);
        for t in inp.images.iter().filter(|t| t.stage == inp.stage) {
            m.next.extend([u64::from(t.slot), t.handle]);
        }
        if m.valid && m.next == m.last {
            return Some(m.addr);
        }
        std::mem::swap(&mut m.last, &mut m.next);
        m.valid = false;
        None
    }

    /// Records the buffer that was built after a miss of [`ArgsMemo::lookup`].
    fn store(&mut self, stage: Stage, addr: u64) {
        let m = match stage {
            Stage::Vertex => &mut self.stages[0],
            Stage::Fragment => &mut self.stages[1],
            Stage::Compute | Stage::Task | Stage::Mesh => return,
        };
        m.addr = addr;
        m.valid = true;
    }
}

/// The per-batch scratch buffer as a sink.
pub(crate) struct ScratchSink<'a> {
    scratch: &'a Scratch,
    uploaded: &'a Mutex<crate::draws::FastMap<usize, u64>>,
    preloaded: Option<&'a crate::draws::FastMap<usize, u64>>,
    memo: Option<&'a mut ArgsMemo>,
}

impl<'a> ScratchSink<'a> {
    /// A sink that writes into `scratch`. `uploaded` remembers the shared snapshots of the batch. A `memo` lets
    /// draws of one pass share argument buffers.
    pub(crate) fn new(
        scratch: &'a Scratch,
        uploaded: &'a Mutex<crate::draws::FastMap<usize, u64>>,
        memo: Option<&'a mut ArgsMemo>,
    ) -> Self {
        Self { scratch, uploaded, preloaded: None, memo }
    }

    /// Looks shared snapshots up in `preloaded` first, without a lock.
    pub(crate) fn with_preloaded(mut self, preloaded: &'a crate::draws::FastMap<usize, u64>) -> Self {
        self.preloaded = Some(preloaded);
        self
    }
}

impl Sink for ScratchSink<'_> {
    fn put_shared(&mut self, data: &Arc<[u8]>) -> Result<u64, Skip> {
        let key = data.as_ptr() as usize;
        if let Some(a) = self.preloaded.and_then(|m| m.get(&key).copied()) {
            return Ok(a);
        }
        if let Some(a) = self.uploaded.lock().ok().and_then(|m| m.get(&key).copied()) {
            return Ok(a);
        }
        let a = self.put(data)?;
        if let Ok(mut m) = self.uploaded.lock() {
            m.insert(key, a);
        }
        Ok(a)
    }

    fn put(&mut self, data: &[u8]) -> Result<u64, Skip> {
        self.scratch.push(data).map_err(|e| crate::buf_skip(format_args!("scratch push {} bytes: {e:?}", data.len())))
    }

    fn memo(&mut self) -> Option<&mut ArgsMemo> {
        self.memo.as_deref_mut()
    }
}

/// A persistent buffer that fills from the front. An ICB keeps its argument buffers here.
pub(crate) struct Arena {
    pub(crate) buffer: Arc<DataBuffer>,
    cursor: u64,
}

impl Arena {
    pub(crate) fn new(ctx: &Context, len: u64) -> Result<Self, Skip> {
        Ok(Self { buffer: Arc::new(DataBuffer::new(ctx, len.max(256)).map_err(|_| Skip::Buffer)?), cursor: 0 })
    }

    /// The offset of a GPU address inside the buffer.
    pub(crate) fn offset_of(&self, addr: u64) -> u64 {
        addr - self.buffer.gpu_address()
    }
}

impl Sink for Arena {
    fn put(&mut self, data: &[u8]) -> Result<u64, Skip> {
        let at = self.cursor.next_multiple_of(16);
        self.buffer.write(at, data).map_err(|_| Skip::Buffer)?;
        self.cursor = at + data.len() as u64;
        Ok(self.buffer.gpu_address() + at)
    }
}

/// The host tables and the sampler cache.
pub(crate) struct Bindless {
    ctx: Context,
    samplers: crate::draws::FastMap<u64, Sampler>,
    default_sampler: Sampler,
    tex_pool: Arc<DataBuffer>,
    samp_pool: Arc<DataBuffer>,
    tex_mirror: crate::draws::FastMap<u32, u64>,
    samp_mirror: crate::draws::FastMap<u32, u64>,
    support: Arc<DataBuffer>,
    zero: Arc<DataBuffer>,
}

/// Bytes of the support buffer and of the zero buffer. 64 KiB is the largest uniform block a shader declares.
const STATIC_BYTES: u64 = 0x10000;
/// The offset of `render_scale[0]` in the support buffer (Ryujinx `SupportBuffer`, each field 16 byte aligned). The
/// support constants follow `nxb-xlate` `abi.rs`.
const SUPPORT_RENDER_SCALE: u64 = 192;
/// The entries of `render_scale`: entry 0 is the scale of the pass, entry `1 + i` the scale of descriptor `i`.
pub(crate) const SUPPORT_SCALES: u32 = 73;
const SUPPORT_RENDER_SCALES: u64 = SUPPORT_SCALES as u64;
/// The bytes of the support buffer the shader may read. `frag_scale_count` (byte 176) stays 0.
const SUPPORT_SIZE: usize = 1392;

fn tsc_key(t: &Tsc) -> u64 {
    let mut h = foldhash::fast::FixedState::default().build_hasher();
    (t.min_filter, t.mag_filter, t.wrap_s, t.wrap_t, t.wrap_r, t.compare_mode, t.compare_func, t.reduction)
        .hash(&mut h);
    for v in [t.lod_min, t.lod_max, t.lod_bias, t.aniso, t.border[0], t.border[1], t.border[2], t.border[3]] {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

impl Bindless {
    pub(crate) fn new(ctx: &Context) -> Result<Self, nxkit_metal::MetalError> {
        let default_sampler = Sampler::new(ctx, &translate::sampler(&DEFAULT_TSC))?;
        let tex_bytes = POOL_HEAD + POOL_ELEMENT * (u64::from(nxkit_nvn::descriptors::MAX_TEXTURE_ID) + 1);
        let samp_bytes = POOL_HEAD + POOL_ELEMENT * (u64::from(nxkit_nvn::descriptors::MAX_SAMPLER_ID) + 1);
        let support = Arc::new(DataBuffer::new(ctx, STATIC_BYTES)?);
        for i in 0..SUPPORT_RENDER_SCALES {
            support.write(SUPPORT_RENDER_SCALE + 16 * i, &1.0f32.to_le_bytes())?;
        }
        let zero = Arc::new(DataBuffer::new(ctx, STATIC_BYTES)?);
        Ok(Self {
            ctx: ctx.clone(),
            samplers: Default::default(),
            default_sampler,
            support,
            zero,
            tex_pool: Arc::new(DataBuffer::new(ctx, tex_bytes)?),
            samp_pool: Arc::new(DataBuffer::new(ctx, samp_bytes)?),
            tex_mirror: Default::default(),
            samp_mirror: Default::default(),
        })
    }

    /// The resource ID of the sampler that a missing sampler header gets.
    pub(crate) fn default_sampler_id(&self) -> u64 {
        self.default_sampler.gpu_resource_id()
    }

    /// The sampler of a header, made once per distinct header.
    pub(crate) fn sampler(&mut self, t: Option<&Tsc>) -> Result<u64, Skip> {
        let Some(t) = t else { return Ok(self.default_sampler_id()) };
        let key = tsc_key(t);
        if let Some(s) = self.samplers.get(&key) {
            return Ok(s.gpu_resource_id());
        }
        let s = Sampler::new(&self.ctx, &translate::sampler(t)).map_err(|_| Skip::Texture)?;
        let id = s.gpu_resource_id();
        self.samplers.insert(key, s);
        Ok(id)
    }

    /// Writes the resource IDs of a handle into the pool tables, when they changed.
    pub(crate) fn publish(&mut self, handle: u64, r: Resolved) {
        let tex = (handle & u64::from(nxkit_nvn::descriptors::MAX_TEXTURE_ID)) as u32;
        let samp = ((handle >> 20) & u64::from(nxkit_nvn::descriptors::MAX_SAMPLER_ID)) as u32;
        if self.tex_mirror.get(&tex) != Some(&r.texture) {
            let at = POOL_HEAD + POOL_ELEMENT * u64::from(tex);
            if self.tex_pool.write(at, &r.texture.to_le_bytes()).is_ok() {
                self.tex_mirror.insert(tex, r.texture);
            }
        }
        if self.samp_mirror.get(&samp) != Some(&r.sampler) {
            let at = POOL_HEAD + POOL_ELEMENT * u64::from(samp);
            if self.samp_pool.write(at, &r.sampler.to_le_bytes()).is_ok() {
                self.samp_mirror.insert(samp, r.sampler);
            }
        }
    }

    /// The addresses of the two pool tables.
    pub(crate) fn pools(&self) -> (u64, u64) {
        (self.tex_pool.gpu_address(), self.samp_pool.gpu_address())
    }

    /// The GPU addresses of the support buffer (render scale 1.0, no BGRA swap) and of a buffer of zeros.
    pub(crate) fn statics(&self) -> (u64, u64) {
        (self.support.gpu_address(), self.zero.gpu_address())
    }

    /// The pool tables, which the command buffers keep alive.
    pub(crate) fn pool_buffers(&self) -> (Arc<DataBuffer>, Arc<DataBuffer>) {
        (self.tex_pool.clone(), self.samp_pool.clone())
    }

    /// Forgets what the tables hold, so that the next frame writes every entry again.
    pub(crate) fn forget(&mut self) {
        self.tex_mirror.clear();
        self.samp_mirror.clear();
    }
}

/// What a stage of one draw or dispatch binds.
/// The constant bank that holds push constants (the translator's `PUSH_BANK`).
const PUSH_BANK: u32 = 24;

pub(crate) struct StageInputs<'a> {
    pub(crate) stage: Stage,
    pub(crate) ubos: &'a [UboDetail],
    pub(crate) sbufs: &'a [UboDetail],
    pub(crate) textures: &'a [TextureBind],
    pub(crate) images: &'a [TextureBind],
    pub(crate) base_instance: u32,
    pub(crate) groups: Option<[u32; 3]>,
    /// The push constants of the draw or dispatch (bank 24), when it has any.
    pub(crate) push: Option<&'a [u8]>,
}

/// What the argument buffers need besides the bindings.
pub(crate) struct Env<'a> {
    pub(crate) handles: &'a crate::draws::FastMap<u64, Resolved>,
    /// The views of [`Handles::fits`].
    pub(crate) fits: &'a crate::draws::FastMap<(u64, nxkit_metal::TextureKind), u64>,
    /// The views of [`Handles::images`].
    pub(crate) images: &'a crate::draws::FastMap<ImageKey, ImageRes>,
    /// The raw global memory table and logs.
    pub(crate) gmem: crate::gmem::GmemEnv<'a>,
    pub(crate) pools: (u64, u64),
    pub(crate) default_sampler: u64,
    /// The support buffer and the zero buffer, from [`Bindless::statics`].
    pub(crate) statics: (u64, u64),
    /// The render scale of a scaled texture (`GpuSettings.scale`, 1 when nothing is scaled).
    pub(crate) scale: f32,
}

fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    if let Some(s) = buf.get_mut(at..at + 8) {
        s.copy_from_slice(&v.to_le_bytes());
    }
}

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    if let Some(s) = buf.get_mut(at..at + 4) {
        s.copy_from_slice(&v.to_le_bytes());
    }
}

/// The driver constant block of a stage. Its layout follows the constants of `nxb-xlate`. The fill is partial:
/// the combined texture handles, the base instance and, for compute, the dispatch size and the uniform buffers.
fn drv_block(b: &mut Vec<u8>, inp: &StageInputs<'_>, ubo_addr: &[(u32, u64, u32)]) {
    b.clear();
    b.resize(DRV_SIZE, 0);
    for t in inp.textures.iter().filter(|t| t.stage == inp.stage) {
        if (t.slot as usize) < DRV_C2_TEX_SLOTS {
            put_u64(b, DRV_C2_TEX + 8 * t.slot as usize, t.handle);
        }
    }
    put_u32(b, DRV_BASE_INSTANCE, inp.base_instance);
    crate::mesh::storage_table(b, inp);
    if let Some(g) = inp.groups {
        for (i, v) in g.iter().enumerate() {
            put_u32(b, DRV_DISPATCH + 4 * i, *v);
        }
        for (slot, addr, size) in ubo_addr {
            if (*slot as usize) < DRV_CS_UBO_SLOTS {
                let at = DRV_CS_UBO + 0x10 * *slot as usize;
                put_u64(b, at, *addr);
                put_u32(b, at + 8, *size);
            }
        }
    }
}

/// The support buffer of a stage, when a render scale is not 1. It holds the scale of the pass in `render_scale[0]` and
/// the scale of each bound texture that the shader scales (`rscale` of its binding). The other entries stay 1.
/// Returns false, and leaves `b` unused, when every scale is 1: the shared buffer of [`Bindless::statics`] serves then.
fn support_block(
    b: &mut Vec<u8>,
    binds: &[crate::programs::Bind],
    env: &Env<'_>,
    pass_scale: f32,
    handle_of: &dyn Fn(u32) -> Option<u64>,
    image_of: &dyn Fn(u32) -> Option<u64>,
) -> bool {
    let entry = |bind: &crate::programs::Bind| -> Option<(u32, f32)> {
        let i = bind.rscale?;
        match bind.kind {
            BindKind::Support => Some((i, pass_scale)),
            BindKind::Tex => {
                let scaled = handle_of(bind.reg).and_then(|h| env.handles.get(&h)).is_some_and(|r| r.scaled);
                Some((i, if scaled { env.scale } else { 1.0 }))
            }
            BindKind::Img => {
                let (h, decl, dim) = (image_of(bind.reg)?, bind.img?, bind.dim?);
                let scaled = env.images.get(&(h, dim, decl.atomic_code())).is_some_and(|r| r.scaled);
                Some((i, if scaled { env.scale } else { 1.0 }))
            }
            _ => None,
        }
    };
    if !binds.iter().filter_map(entry).any(|(_, v)| v != 1.0) {
        return false;
    }
    b.clear();
    b.resize(SUPPORT_SIZE, 0);
    for i in 0..SUPPORT_RENDER_SCALES {
        put_f32(b, (SUPPORT_RENDER_SCALE + 16 * i) as usize, 1.0);
    }
    for (i, v) in binds.iter().filter_map(entry) {
        put_f32(b, (SUPPORT_RENDER_SCALE + 16 * u64::from(i)) as usize, v);
    }
    true
}

fn put_f32(buf: &mut [u8], at: usize, v: f32) {
    put_u32(buf, at, v.to_bits());
}

/// The buffers that [`build_args`] reuses from draw to draw.
#[derive(Default)]
struct ArgsScratch {
    args: Vec<u8>,
    drv: Vec<u8>,
    support: Vec<u8>,
    ubos: Vec<(u32, u64, u32)>,
}

thread_local! {
    static ARGS_SCRATCH: std::cell::Cell<ArgsScratch> = std::cell::Cell::new(ArgsScratch::default());
}

/// Builds the argument buffer of one stage and puts it into `sink`, together with the bytes of the uniform buffers
/// that the guest rewrote in the same frame and the driver block. When the sink keeps an [`ArgsMemo`] and the
/// previous draw of the stage bound the same inputs, the buffer of that draw is returned and nothing is written.
pub(crate) fn fill_args(
    p: &StageProgram,
    inp: &StageInputs<'_>,
    env: &Env<'_>,
    sink: &mut dyn Sink,
) -> Result<u64, Skip> {
    fill_args_scaled(p, inp, env, 1.0, sink)
}

/// [`fill_args`] for a stage of a render pass whose attachments have the render scale `pass_scale`. A pixel shader
/// that divides its pixel position by the scale of the pass reads it from the support buffer.
pub(crate) fn fill_args_scaled(
    p: &StageProgram,
    inp: &StageInputs<'_>,
    env: &Env<'_>,
    pass_scale: f32,
    sink: &mut dyn Sink,
) -> Result<u64, Skip> {
    if let Some(a) = sink.memo().and_then(|m| m.lookup(p, inp, pass_scale)) {
        return Ok(a);
    }
    let addr = build_args(p, inp, env, pass_scale, sink)?;
    if let Some(m) = sink.memo() {
        m.store(inp.stage, addr);
    }
    Ok(addr)
}

fn build_args(
    p: &StageProgram,
    inp: &StageInputs<'_>,
    env: &Env<'_>,
    pass_scale: f32,
    sink: &mut dyn Sink,
) -> Result<u64, Skip> {
    let mut scratch = ARGS_SCRATCH.take();
    let r = build_args_in(p, inp, env, pass_scale, sink, &mut scratch);
    ARGS_SCRATCH.set(scratch);
    r
}

fn build_args_in(
    p: &StageProgram,
    inp: &StageInputs<'_>,
    env: &Env<'_>,
    pass_scale: f32,
    sink: &mut dyn Sink,
    scratch: &mut ArgsScratch,
) -> Result<u64, Skip> {
    let ArgsScratch { args, drv, support, ubos } = scratch;
    args.clear();
    args.resize((p.arg_bytes as usize).max(8), 0);
    ubos.clear();
    for u in inp.ubos.iter().filter(|u| u.stage == inp.stage) {
        let addr = match &u.snapshot {
            Some(bytes) => sink.put_shared(bytes)?,
            None => u.address,
        };
        ubos.push((u.slot, addr, u.size));
    }
    let handle_of = |slot: u32| inp.textures.iter().find(|t| t.stage == inp.stage && t.slot == slot).map(|t| t.handle);
    let image_of = |slot: u32| inp.images.iter().find(|t| t.stage == inp.stage && t.slot == slot).map(|t| t.handle);
    for b in &p.binds {
        let at = 8 * b.arg_id as usize;
        let v = match b.kind {
            BindKind::Cbuf => ubos.iter().find(|(s, _, _)| *s == b.reg).map_or(env.statics.1, |(_, a, _)| *a),
            BindKind::Tex => handle_of(b.reg).map_or(0, |h| {
                b.dim
                    .and_then(|k| env.fits.get(&(h, k)).copied())
                    .or_else(|| env.handles.get(&h).map(|r| r.texture))
                    .unwrap_or(0)
            }),
            BindKind::Samp | BindKind::SampTex => handle_of(b.reg)
                .and_then(|h| env.handles.get(&h))
                .map_or(env.default_sampler, |r| r.sampler),
            BindKind::TexPool => env.pools.0,
            BindKind::SampPool => env.pools.1,
            // Bank 24 is the push constant block; banks 0 and 2 are the driver block.
            BindKind::DrvCb if b.bank == PUSH_BANK => match inp.push {
                Some(push) => sink.put(push)?,
                None => env.statics.1,
            },
            BindKind::DrvCb => {
                drv_block(drv, inp, ubos);
                sink.put(drv)?
            }
            BindKind::UboPtr => ubos.iter().find(|(s, _, _)| *s == b.reg).map_or(env.statics.1, |(_, a, _)| *a),
            BindKind::Support => {
                if support_block(support, &p.binds, env, pass_scale, &handle_of, &image_of) {
                    sink.put(support)?
                } else {
                    env.statics.0
                }
            }
            // A zero border block has no sampler bit set, so no sample is corrected.
            BindKind::Border => env.statics.1,
            // A storage buffer that is not bound reads zeros, and its writes land in the zero buffer. A buffer that the
            // shader only reads takes the walk's copy when there is one (the overlap: the CPU may already write the next
            // frame's data); one that it may write keeps the buffer itself, so the writes stay.
            BindKind::Sbuf => match inp.sbufs.iter().find(|s| s.stage == inp.stage && s.slot == b.reg) {
                Some(s) => match &s.snapshot {
                    Some(bytes) if !b.rw => sink.put_shared(bytes)?,
                    _ => s.address,
                },
                None => env.statics.1,
            },
            BindKind::Img => {
                note_n(Reason::BindImg, 1);
                let found = b.dim.zip(b.img).and_then(|(dim, decl)| {
                    let h = image_of(b.reg)?;
                    env.images.get(&(h, dim, decl.atomic_code())).map(|r| r.id)
                });
                match found {
                    Some(id) if id != 0 => id,
                    _ => {
                        note_n(Reason::ImgUnbound, 1);
                        0
                    }
                }
            }
            BindKind::GmemMap => {
                note_n(Reason::BindGmemMap, 1);
                if env.gmem.map != 0 { env.gmem.map } else { env.statics.1 }
            }
            BindKind::GmemLog => {
                note_n(Reason::BindGmemLog, 1);
                match env.gmem.log() {
                    0 => env.statics.1,
                    a => a,
                }
            }
            BindKind::C1 if p.c1.is_empty() => env.statics.1,
            BindKind::C1 => sink.put(&p.c1)?,
        };
        put_u64(args, at, v);
    }
    sink.put(args)
}

#[cfg(test)]
mod support_tests {
    use super::*;
    use crate::programs::Bind;

    fn bind(kind: BindKind, reg: u32, rscale: Option<u32>) -> Bind {
        Bind { arg_id: 0, bank: 0, kind, reg, dim: None, rw: false, rscale, img: None }
    }

    fn env<'a>(
        handles: &'a crate::draws::FastMap<u64, Resolved>,
        fits: &'a crate::draws::FastMap<(u64, nxkit_metal::TextureKind), u64>,
        images: &'a crate::draws::FastMap<ImageKey, ImageRes>,
    ) -> Env<'a> {
        Env { handles, fits, images, gmem: crate::gmem::GmemEnv::NONE, pools: (0, 0), default_sampler: 0, statics: (0, 0), scale: 2.0 }
    }

    fn entry(b: &[u8], i: usize) -> f32 {
        let at = SUPPORT_RENDER_SCALE as usize + 16 * i;
        f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    #[test]
    fn native_size_needs_no_block() {
        let (h, f, i) = Default::default();
        let binds = [bind(BindKind::Support, 0, Some(0)), bind(BindKind::Tex, 3, Some(1))];
        let mut b = Vec::new();
        assert!(!support_block(&mut b, &binds, &env(&h, &f, &i), 1.0, &|_| None, &|_| None));
    }

    #[test]
    fn the_pass_and_the_scaled_texture_get_their_entries() {
        let mut h: crate::draws::FastMap<u64, Resolved> = Default::default();
        h.insert(7, Resolved { texture: 1, sampler: 1, scaled: true });
        h.insert(8, Resolved { texture: 2, sampler: 2, scaled: false });
        let (f, i) = Default::default();
        let binds = [
            bind(BindKind::Support, 0, Some(0)),
            bind(BindKind::Tex, 3, Some(1)),
            bind(BindKind::Tex, 4, Some(2)),
            bind(BindKind::Tex, 5, None),
        ];
        let handle = |slot: u32| match slot {
            3 => Some(7),
            4 => Some(8),
            _ => None,
        };
        let mut b = Vec::new();
        assert!(support_block(&mut b, &binds, &env(&h, &f, &i), 2.0, &handle, &|_| None));
        assert_eq!(b.len(), SUPPORT_SIZE);
        assert_eq!((entry(&b, 0), entry(&b, 1), entry(&b, 2), entry(&b, 3)), (2.0, 2.0, 1.0, 1.0));
        // frag_scale_count stays 0
        assert_eq!(&b[176..180], &[0; 4]);
    }

    #[test]
    fn a_scaled_image_gets_its_entry() {
        use crate::programs::{ImgAccess, ImgDecl};
        use nxkit_metal::TextureKind;
        let decl = ImgDecl { access: ImgAccess::Write, atomic: false, signed: false };
        let mut img = bind(BindKind::Img, 0, Some(3));
        img.dim = Some(TextureKind::D2);
        img.img = Some(decl);
        let (h, f) = Default::default();
        let mut images: crate::draws::FastMap<ImageKey, ImageRes> = Default::default();
        images.insert((5, TextureKind::D2, 0), ImageRes { id: 9, scaled: true });
        let mut b = Vec::new();
        let at = |slot: u32| (slot == 0).then_some(5);
        assert!(support_block(&mut b, &[img], &env(&h, &f, &images), 1.0, &|_| None, &at));
        assert_eq!(entry(&b, 3), 2.0);
        // an image nothing is bound to keeps scale 1
        assert!(!support_block(&mut b, &[img], &env(&h, &f, &images), 1.0, &|_| None, &|_| None));
    }
}
