//! Bindless textures and samplers.
//!
//! A translated shader reads its textures and samplers from one argument buffer per stage (the host contract of D20).
//! A member of that buffer holds a resource ID. The game binds a texture handle (`texture_id | sampler_id << 20`).
//! The encoder resolves the handle to the Metal texture and sampler and writes their resource IDs into the argument
//! buffer, so a draw needs no bind call per texture. Shaders that index a pool read two host tables, one of texture
//! resource IDs by descriptor id and one of sampler resource IDs by sampler id.

mod args;
pub(crate) mod marks;
#[cfg(test)]
mod pool_tests;

pub(crate) use self::args::{Env, StageInputs, fill_args, fill_args_scaled};

use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use nxkit_metal::{Context, DataBuffer, Sampler, Scratch, Texture};
use nxkit_nvn::resource::TexId;
use nxkit_nvn::tic::Tsc;

use crate::detail::{Stage, TextureBind, UboDetail};
use crate::programs::{BindKind, StageProgram};
use crate::stats::Skip;
use crate::translate;
use nxkit_nvn::diag::{Reason, note_n};

/// The bytes before the first element of a pool table. The layout is the contract of `nxb-xlate` (`msl/pool.rs`:
/// `strukt`, `element` and `MARK`): `n` (u32) at byte 0 is the highest ID that a read can name, `seen` (u64) at byte 8 is
/// the GPU address of the bitmap of the IDs that shaders read, and the elements (8 byte resource IDs) start at byte 16.
pub(crate) const POOL_HEAD: u64 = 16;
/// The byte of `seen` in the head of a table.
const POOL_SEEN: u64 = 8;
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
    /// The layout block of the last pulled draw and its address (`crate::pull`).
    layout: Option<(crate::pull::Block, u64)>,
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
        m.next.push(u64::from(inp.blendz));
        // The push constants are shared until the game pushes again, so their address names their content.
        m.next.push(inp.push.map_or(0, |b| b.as_ptr() as u64));
        let mark = m.next.len();
        m.next.push(0);
        for u in inp.ubos.iter().filter(|u| u.stage == inp.stage) {
            let snapshot = u.snapshot.as_ref().map_or(0, |b| b.as_ptr() as u64);
            m.next
                .extend([u64::from(u.slot), snapshot, u.address, u64::from(u.size)]);
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

    /// The address of `block` when the last pulled draw put the same block.
    pub(crate) fn layout(&self, block: &crate::pull::Block) -> Option<u64> {
        self.layout
            .as_ref()
            .filter(|(b, _)| b == block)
            .map(|(_, a)| *a)
    }

    /// Records the address of the layout block that a pulled draw put.
    pub(crate) fn store_layout(&mut self, block: &crate::pull::Block, addr: u64) {
        self.layout = Some((*block, addr));
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
        Self {
            scratch,
            uploaded,
            preloaded: None,
            memo,
        }
    }

    /// Looks shared snapshots up in `preloaded` first, without a lock.
    pub(crate) fn with_preloaded(
        mut self,
        preloaded: &'a crate::draws::FastMap<usize, u64>,
    ) -> Self {
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
        self.scratch
            .push(data)
            .map_err(|e| crate::buf_skip(format_args!("scratch push {} bytes: {e:?}", data.len())))
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
        Ok(Self {
            buffer: Arc::new(DataBuffer::new(ctx, len.max(256)).map_err(|_| Skip::Buffer)?),
            cursor: 0,
        })
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
    /// The bitmap of the texture IDs that shaders read (`seen` of the texture table), and of the sampler IDs, which no
    /// generated code marks: it only makes `seen` of the sampler table an address that is valid.
    tex_seen: Arc<DataBuffer>,
    samp_seen: Arc<DataBuffer>,
    tex_mirror: crate::draws::FastMap<u32, u64>,
    samp_mirror: crate::draws::FastMap<u32, u64>,
    /// The texture and Metal texture (store generation) behind each element of the texture table that is not 0.
    tex_owner: crate::draws::FastMap<u32, (TexId, u64)>,
    /// `n` of the two tables as written.
    tex_n: u32,
    samp_n: u32,
    marks: marks::Marks,
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
    (
        t.min_filter,
        t.mag_filter,
        t.wrap_s,
        t.wrap_t,
        t.wrap_r,
        t.compare_mode,
        t.compare_func,
        t.reduction,
    )
        .hash(&mut h);
    for v in [
        t.lod_min,
        t.lod_max,
        t.lod_bias,
        t.aniso,
        t.border[0],
        t.border[1],
        t.border[2],
        t.border[3],
    ] {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

impl Bindless {
    pub(crate) fn new(ctx: &Context) -> Result<Self, nxkit_metal::MetalError> {
        let default_sampler = Sampler::new(ctx, &translate::sampler(&DEFAULT_TSC))?;
        let tex_bytes =
            POOL_HEAD + POOL_ELEMENT * (u64::from(nxkit_nvn::descriptors::MAX_TEXTURE_ID) + 1);
        let samp_bytes =
            POOL_HEAD + POOL_ELEMENT * (u64::from(nxkit_nvn::descriptors::MAX_SAMPLER_ID) + 1);
        let tex_seen = Arc::new(DataBuffer::new(
            ctx,
            (u64::from(nxkit_nvn::descriptors::MAX_TEXTURE_ID) + 1) / 8,
        )?);
        let samp_seen = Arc::new(DataBuffer::new(
            ctx,
            (u64::from(nxkit_nvn::descriptors::MAX_SAMPLER_ID) + 1) / 8,
        )?);
        let tex_pool = Arc::new(DataBuffer::new(ctx, tex_bytes)?);
        let samp_pool = Arc::new(DataBuffer::new(ctx, samp_bytes)?);
        tex_pool.write(POOL_SEEN, &tex_seen.gpu_address().to_le_bytes())?;
        samp_pool.write(POOL_SEEN, &samp_seen.gpu_address().to_le_bytes())?;
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
            tex_pool,
            samp_pool,
            tex_seen,
            samp_seen,
            tex_mirror: Default::default(),
            samp_mirror: Default::default(),
            tex_owner: Default::default(),
            tex_n: 0,
            samp_n: 0,
            marks: Default::default(),
        })
    }

    /// The resource ID of the sampler that a missing sampler header gets.
    pub(crate) fn default_sampler_id(&self) -> u64 {
        self.default_sampler.gpu_resource_id()
    }

    /// The sampler of a header, made once per distinct header.
    pub(crate) fn sampler(&mut self, t: Option<&Tsc>) -> Result<u64, Skip> {
        let Some(t) = t else {
            return Ok(self.default_sampler_id());
        };
        let key = tsc_key(t);
        if let Some(s) = self.samplers.get(&key) {
            return Ok(s.gpu_resource_id());
        }
        let s = Sampler::new(&self.ctx, &translate::sampler(t)).map_err(|_| Skip::Texture)?;
        let id = s.gpu_resource_id();
        self.samplers.insert(key, s);
        Ok(id)
    }

    /// Writes the resource IDs of a handle into the pool tables, when they changed. `owner` is the texture of the
    /// handle and the generation of its Metal texture in the store: the element is cleared when either goes
    /// ([`Bindless::release_gone`]).
    pub(crate) fn publish(&mut self, handle: u64, r: Resolved, owner: (TexId, u64)) {
        let tex = (handle & u64::from(nxkit_nvn::descriptors::MAX_TEXTURE_ID)) as u32;
        let samp = ((handle >> 20) & u64::from(nxkit_nvn::descriptors::MAX_SAMPLER_ID)) as u32;
        self.publish_texture(tex, r.texture, owner);
        self.publish_sampler(samp, r.sampler);
    }

    /// Writes the element of texture ID `id`, when it changed, and raises `n` to `id` when that is higher.
    pub(crate) fn publish_texture(&mut self, id: u32, resource: u64, owner: (TexId, u64)) -> bool {
        let at = POOL_HEAD + POOL_ELEMENT * u64::from(id);
        let changed = self.tex_mirror.get(&id) != Some(&resource);
        if changed && self.tex_pool.write(at, &resource.to_le_bytes()).is_ok() {
            self.tex_mirror.insert(id, resource);
        } else if changed {
            return false;
        }
        self.tex_owner.insert(id, owner);
        Self::raise(&self.tex_pool, &mut self.tex_n, id);
        changed
    }

    /// Writes the element of sampler ID `id`, when it changed, and raises `n` to `id` when that is higher.
    pub(crate) fn publish_sampler(&mut self, id: u32, resource: u64) {
        if self.samp_mirror.get(&id) != Some(&resource) {
            let at = POOL_HEAD + POOL_ELEMENT * u64::from(id);
            if self.samp_pool.write(at, &resource.to_le_bytes()).is_ok() {
                self.samp_mirror.insert(id, resource);
            }
        }
        Self::raise(&self.samp_pool, &mut self.samp_n, id);
    }

    /// Raises `n` of the texture table to the highest ID a read can name: the last slot of the texture pools. The
    /// elements past the IDs that the host filled stay 0, so a read there gets the null texture, and its bit in the
    /// bitmap tells the host to fill it. It never shrinks.
    pub(crate) fn raise_texture_limit(&mut self, limit: u32) {
        Self::raise(&self.tex_pool, &mut self.tex_n, limit);
    }

    fn raise(table: &DataBuffer, n: &mut u32, id: u32) {
        if id > *n && table.write(0, &id.to_le_bytes()).is_ok() {
            *n = id;
        }
    }

    /// Sets the element of texture ID `id` to 0 and forgets it. Returns true when it held a resource ID.
    pub(crate) fn clear_texture(&mut self, id: u32) -> bool {
        self.tex_owner.remove(&id);
        let Some(held) = self.tex_mirror.get(&id).map(|r| *r != 0) else {
            return false;
        };
        let at = POOL_HEAD + POOL_ELEMENT * u64::from(id);
        if self.tex_pool.write(at, &0u64.to_le_bytes()).is_ok() {
            self.tex_mirror.remove(&id);
        }
        held
    }

    /// Sets to 0 every element of the texture table whose texture `gone` names (by the texture and the generation of
    /// its Metal texture), so that no element holds the resource ID of a texture or view that was dropped. Returns how
    /// many elements it cleared.
    pub(crate) fn release_gone(&mut self, mut gone: impl FnMut(TexId, u64) -> bool) -> u32 {
        let stale: Vec<u32> = self
            .tex_owner
            .iter()
            .filter(|(_, (t, g))| gone(*t, *g))
            .map(|(id, _)| *id)
            .collect();
        let mut n = 0;
        for id in stale {
            n += u32::from(self.clear_texture(id));
        }
        n
    }

    /// The elements of the texture table that hold a resource ID, with their owner. For tests and diagnostics.
    #[cfg(test)]
    pub(crate) fn owners(&self) -> usize {
        self.tex_owner.len()
    }

    /// Reads the bitmap of the texture IDs up to `limit`, sweeps it when it is time and returns the wanted IDs in
    /// ascending order ([`marks::Marks`]).
    pub(crate) fn wanted(&mut self, limit: u32) -> Vec<u32> {
        let words = ((limit >> 5) as usize + 1).min(self.tex_seen.len() as usize / 4);
        let bits = self.tex_seen.clone();
        // A word that cannot be read counts as 0.
        self.marks.step(
            words,
            |w| bits.load_u32(4 * w as u64).unwrap_or(0),
            |w, b| {
                let _ = bits.fetch_and_u32(4 * w as u64, !b);
            },
        )
    }

    /// The addresses of the two pool tables.
    pub(crate) fn pools(&self) -> (u64, u64) {
        (self.tex_pool.gpu_address(), self.samp_pool.gpu_address())
    }

    /// The GPU addresses of the support buffer (render scale 1.0, no BGRA swap) and of a buffer of zeros.
    pub(crate) fn statics(&self) -> (u64, u64) {
        (self.support.gpu_address(), self.zero.gpu_address())
    }

    /// The pool tables and their bitmaps, which the command buffers keep alive. They join the residency set when they are
    /// made, like every `DataBuffer`.
    pub(crate) fn pool_buffers(&self) -> Vec<Arc<DataBuffer>> {
        vec![
            self.tex_pool.clone(),
            self.samp_pool.clone(),
            self.tex_seen.clone(),
            self.samp_seen.clone(),
        ]
    }

    /// The GPU addresses of the two bitmaps (`seen`).
    #[cfg(test)]
    pub(crate) fn seen(&self) -> (u64, u64) {
        (self.tex_seen.gpu_address(), self.samp_seen.gpu_address())
    }
}
