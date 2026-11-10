//! The argument buffers of a stage: the driver block, the support block and the texture and uniform buffer tables.

use super::*;
use crate::pools::{Backing, PoolSnapshot};

#[cfg(test)]
mod tests;

/// What a stage of one draw or dispatch binds.
/// The constant bank that holds push constants (the translator's `PUSH_BANK`).
pub(crate) const PUSH_BANK: u32 = 24;

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
    /// The blend word of a fragment stage (`translate::blend_zero_word`), 0 for every other stage.
    pub(crate) blendz: u32,
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
    /// The storage ranges that this frame reads from a copy of its own (`rename`).
    pub(crate) rename: &'a crate::rename::RenameMap,
    /// The memory pools at the flush of the frame.
    pub(crate) guest_pools: &'a crate::guestpools::GuestPools,
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
fn drv_block(b: &mut Vec<u8>, inp: &StageInputs<'_>, ubos: &[UboWord]) {
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
        // The table stays virtual: it holds the guest address, not the word the shader dereferences.
        for u in ubos {
            if (u.slot as usize) < DRV_CS_UBO_SLOTS {
                let at = DRV_CS_UBO + 0x10 * u.slot as usize;
                put_u64(b, at, u.guest);
                put_u32(b, at + 8, u.size);
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
                let scaled = handle_of(bind.reg)
                    .and_then(|h| env.handles.get(&h))
                    .is_some_and(|r| r.scaled);
                Some((i, if scaled { env.scale } else { 1.0 }))
            }
            BindKind::Img => {
                let (h, decl, dim) = (image_of(bind.reg)?, bind.img?, bind.dim?);
                let scaled = env
                    .images
                    .get(&(h, dim, decl.atomic_code()))
                    .is_some_and(|r| r.scaled);
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

/// A uniform buffer of a stage as the argument buffer and the driver block see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UboWord {
    slot: u32,
    /// The address a shader dereferences: a real address, or the scratch copy of a snapshot.
    word: u64,
    /// The guest address for the c0 table of a compute stage (0 when the buffer is outside every pool).
    guest: u64,
    /// The size for the c0 table, cut to the end of its pool.
    size: u32,
}

/// The real address of a guest range and its size cut to the room of its pool. A range in no pool is counted and
/// gives `None`, so the caller binds the zero buffer as for an unbound slot.
fn locate<B: Backing>(pools: &PoolSnapshot<B>, guest: u64, size: u32) -> Option<(u64, u32)> {
    match pools.resolve_start(guest) {
        Ok(loc) => Some((
            loc.real,
            size.min(u32::try_from(loc.room).unwrap_or(u32::MAX)),
        )),
        Err(_) => {
            note_n(Reason::BindNoPool, 1);
            None
        }
    }
}

/// Puts a snapshot into the scratch buffer and gives its address.
type PutShared<'a> = &'a mut dyn FnMut(&Arc<[u8]>) -> Result<u64, Skip>;

/// The uniform buffers of `stage`. A buffer with a snapshot reads its scratch copy and keeps its guest address for
/// c0; the others read through their real address. A buffer in no pool reads the zero buffer.
fn ubo_words<B: Backing>(
    out: &mut Vec<UboWord>,
    pools: &PoolSnapshot<B>,
    stage: Stage,
    ubos: &[UboDetail],
    zero: u64,
    put: PutShared<'_>,
) -> Result<(), Skip> {
    out.clear();
    for u in ubos.iter().filter(|u| u.stage == stage) {
        let w = match &u.snapshot {
            Some(bytes) => UboWord {
                slot: u.slot,
                word: put(bytes)?,
                guest: u.address,
                size: u.size,
            },
            None => match locate(pools, u.address, u.size) {
                Some((real, size)) => UboWord {
                    slot: u.slot,
                    word: real,
                    guest: u.address,
                    size,
                },
                None => UboWord {
                    slot: u.slot,
                    word: zero,
                    guest: 0,
                    size: 0,
                },
            },
        };
        out.push(w);
    }
    Ok(())
}

/// The word of a storage buffer. One that is not bound reads zeros, and its writes land in the zero buffer. A range that
/// the frame renamed reads and writes its copy (`rename`). A buffer that the shader only reads takes the walk's copy when there is one (the overlap: the CPU may already write the
/// next frame's data); one that it may write keeps the buffer itself, so the writes stay.
fn sbuf_word<B: Backing>(
    pools: &PoolSnapshot<B>,
    rename: &crate::rename::RenameMap,
    bound: Option<&UboDetail>,
    rw: bool,
    zero: u64,
    put: PutShared<'_>,
) -> Result<u64, Skip> {
    let Some(s) = bound else {
        return Ok(zero);
    };
    // A range that the frame renamed reads its copy, read-only or not: the copy holds what the CPU wrote at the flush.
    if let Some(real) = rename.resolve(s.address, s.size) {
        return Ok(real);
    }
    match &s.snapshot {
        Some(bytes) if !rw => put(bytes),
        _ => Ok(locate(pools, s.address, s.size).map_or(zero, |(real, _)| real)),
    }
}

/// The buffers that [`build_args`] reuses from draw to draw.
#[derive(Default)]
struct ArgsScratch {
    args: Vec<u8>,
    drv: Vec<u8>,
    support: Vec<u8>,
    ubos: Vec<UboWord>,
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

/// The blend word: the zero buffer when the word is 0 (blending off), else a copy of the 4 bytes.
fn blend_word(word: u32, zero: u64, sink: &mut dyn Sink) -> Result<u64, Skip> {
    if word == 0 {
        Ok(zero)
    } else {
        sink.put(&word.to_le_bytes())
    }
}

fn build_args_in(
    p: &StageProgram,
    inp: &StageInputs<'_>,
    env: &Env<'_>,
    pass_scale: f32,
    sink: &mut dyn Sink,
    scratch: &mut ArgsScratch,
) -> Result<u64, Skip> {
    let ArgsScratch {
        args,
        drv,
        support,
        ubos,
    } = scratch;
    args.clear();
    args.resize((p.arg_bytes as usize).max(8), 0);
    ubo_words(
        ubos,
        env.guest_pools,
        inp.stage,
        inp.ubos,
        env.statics.1,
        &mut |bytes| sink.put_shared(bytes),
    )?;
    let handle_of = |slot: u32| {
        inp.textures
            .iter()
            .find(|t| t.stage == inp.stage && t.slot == slot)
            .map(|t| t.handle)
    };
    let image_of = |slot: u32| {
        inp.images
            .iter()
            .find(|t| t.stage == inp.stage && t.slot == slot)
            .map(|t| t.handle)
    };
    for b in &p.binds {
        let at = 8 * b.arg_id as usize;
        let v = match b.kind {
            BindKind::Cbuf => ubos
                .iter()
                .find(|u| u.slot == b.reg)
                .map_or(env.statics.1, |u| u.word),
            BindKind::Tex => handle_of(b.reg).map_or(0, |h| {
                b.dim
                    .and_then(|k| env.fits.get(&(h, k)).copied())
                    .or_else(|| env.handles.get(&h).map(|r| r.texture))
                    .unwrap_or(0)
            }),
            BindKind::Samp | BindKind::SampTex => handle_of(b.reg)
                .and_then(|h| env.handles.get(&h))
                .map_or(env.default_sampler, |r| r.sampler),
            BindKind::TexPool => {
                note_n(Reason::BindTexPool, 1);
                env.pools.0
            }
            BindKind::SampPool => {
                note_n(Reason::BindSampPool, 1);
                env.pools.1
            }
            // Bank 24 is the push constant block; banks 0 and 2 are the driver block.
            BindKind::DrvCb if b.bank == PUSH_BANK => match inp.push {
                Some(push) => sink.put(push)?,
                None => env.statics.1,
            },
            BindKind::DrvCb => {
                drv_block(drv, inp, ubos);
                sink.put(drv)?
            }
            BindKind::UboPtr => ubos
                .iter()
                .find(|u| u.slot == b.reg)
                .map_or(env.statics.1, |u| u.word),
            BindKind::Support => {
                if support_block(support, &p.binds, env, pass_scale, &handle_of, &image_of) {
                    sink.put(support)?
                } else {
                    env.statics.0
                }
            }
            // A zero border block has no sampler bit set, so no sample is corrected.
            BindKind::Border => env.statics.1,
            BindKind::BlendZ => blend_word(inp.blendz, env.statics.1, sink)?,
            BindKind::Sbuf => sbuf_word(
                env.guest_pools,
                env.rename,
                inp.sbufs
                    .iter()
                    .find(|s| s.stage == inp.stage && s.slot == b.reg),
                b.rw,
                env.statics.1,
                &mut |bytes| sink.put_shared(bytes),
            )?,
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
                if env.gmem.map != 0 {
                    env.gmem.map
                } else {
                    env.statics.1
                }
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
