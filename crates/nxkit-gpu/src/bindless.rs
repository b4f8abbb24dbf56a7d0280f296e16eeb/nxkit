//! Bindless textures and samplers.
//!
//! A translated shader reads its textures and samplers from one argument buffer per stage (the host contract of D20).
//! A member of that buffer holds a resource ID. The game binds a texture handle (`texture_id | sampler_id << 20`).
//! The encoder resolves the handle to the Metal texture and sampler and writes their resource IDs into the argument
//! buffer, so a draw needs no bind call per texture. Shaders that index a pool read two host tables, one of texture
//! resource IDs by descriptor id and one of sampler resource IDs by sampler id.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use nxkit_metal::{Context, DataBuffer, Sampler, Scratch, Texture};
use nxkit_nvn::tic::Tsc;

use crate::detail::{Stage, TextureBind, UboDetail};
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
}

/// The handles that a frame resolved, and the Metal objects behind them. The command buffers keep the textures alive.
#[derive(Default)]
pub(crate) struct Handles {
    pub(crate) map: HashMap<u64, Resolved>,
    pub(crate) keep: Vec<Texture>,
}

/// A place that takes bytes and returns the GPU address of the copy.
pub(crate) trait Sink {
    fn put(&mut self, data: &[u8]) -> Result<u64, Skip>;
}

/// The per-batch scratch buffer as a sink.
pub(crate) struct ScratchSink<'a>(pub(crate) &'a Scratch);

impl Sink for ScratchSink<'_> {
    fn put(&mut self, data: &[u8]) -> Result<u64, Skip> {
        self.0.push(data).map_err(|_| Skip::Buffer)
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
    samplers: HashMap<u64, Sampler>,
    default_sampler: Sampler,
    tex_pool: Arc<DataBuffer>,
    samp_pool: Arc<DataBuffer>,
    tex_mirror: HashMap<u32, u64>,
    samp_mirror: HashMap<u32, u64>,
}

fn tsc_key(t: &Tsc) -> u64 {
    let mut h = DefaultHasher::new();
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
        Ok(Self {
            ctx: ctx.clone(),
            samplers: HashMap::new(),
            default_sampler,
            tex_pool: Arc::new(DataBuffer::new(ctx, tex_bytes)?),
            samp_pool: Arc::new(DataBuffer::new(ctx, samp_bytes)?),
            tex_mirror: HashMap::new(),
            samp_mirror: HashMap::new(),
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
pub(crate) struct StageInputs<'a> {
    pub(crate) stage: Stage,
    pub(crate) ubos: &'a [UboDetail],
    pub(crate) textures: &'a [TextureBind],
    pub(crate) base_instance: u32,
    pub(crate) groups: Option<[u32; 3]>,
}

/// What the argument buffers need besides the bindings.
pub(crate) struct Env<'a> {
    pub(crate) handles: &'a HashMap<u64, Resolved>,
    pub(crate) pools: (u64, u64),
    pub(crate) default_sampler: u64,
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
fn drv_block(inp: &StageInputs<'_>, ubo_addr: &[(u32, u64, u32)]) -> Vec<u8> {
    let mut b = vec![0u8; DRV_SIZE];
    for t in inp.textures.iter().filter(|t| t.stage == inp.stage) {
        if (t.slot as usize) < DRV_C2_TEX_SLOTS {
            put_u64(&mut b, DRV_C2_TEX + 8 * t.slot as usize, t.handle);
        }
    }
    put_u32(&mut b, DRV_BASE_INSTANCE, inp.base_instance);
    if let Some(g) = inp.groups {
        for (i, v) in g.iter().enumerate() {
            put_u32(&mut b, DRV_DISPATCH + 4 * i, *v);
        }
        for (slot, addr, size) in ubo_addr {
            if (*slot as usize) < DRV_CS_UBO_SLOTS {
                let at = DRV_CS_UBO + 0x10 * *slot as usize;
                put_u64(&mut b, at, *addr);
                put_u32(&mut b, at + 8, *size);
            }
        }
    }
    b
}

/// Builds the argument buffer of one stage and puts it into `sink`, together with the bytes of the uniform buffers
/// that the guest rewrote in the same frame and the driver block.
pub(crate) fn fill_args(
    p: &StageProgram,
    inp: &StageInputs<'_>,
    env: &Env<'_>,
    sink: &mut dyn Sink,
) -> Result<u64, Skip> {
    let mut args = vec![0u8; (p.arg_bytes as usize).max(8)];
    let mut ubos: Vec<(u32, u64, u32)> = Vec::new();
    for u in inp.ubos.iter().filter(|u| u.stage == inp.stage) {
        let addr = match &u.snapshot {
            Some(bytes) => sink.put(bytes)?,
            None => u.address,
        };
        ubos.push((u.slot, addr, u.size));
    }
    let handle_of = |slot: u32| inp.textures.iter().find(|t| t.stage == inp.stage && t.slot == slot).map(|t| t.handle);
    for b in &p.binds {
        let at = 8 * b.arg_id as usize;
        let v = match b.kind {
            BindKind::Cbuf => ubos.iter().find(|(s, _, _)| *s == b.reg).map_or(0, |(_, a, _)| *a),
            BindKind::Tex => handle_of(b.reg).and_then(|h| env.handles.get(&h)).map_or(0, |r| r.texture),
            BindKind::Samp | BindKind::SampTex => handle_of(b.reg)
                .and_then(|h| env.handles.get(&h))
                .map_or(env.default_sampler, |r| r.sampler),
            BindKind::TexPool => env.pools.0,
            BindKind::SampPool => env.pools.1,
            BindKind::DrvCb => sink.put(&drv_block(inp, &ubos))?,
        };
        put_u64(&mut args, at, v);
    }
    sink.put(&args)
}
