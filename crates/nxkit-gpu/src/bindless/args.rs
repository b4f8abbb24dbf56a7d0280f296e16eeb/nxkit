//! The argument buffers of a stage: the driver block, the support block and the texture and uniform buffer tables.

use super::*;

#[cfg(test)]
mod tests;

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
    let ArgsScratch {
        args,
        drv,
        support,
        ubos,
    } = scratch;
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
                .find(|(s, _, _)| *s == b.reg)
                .map_or(env.statics.1, |(_, a, _)| *a),
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
            BindKind::UboPtr => ubos
                .iter()
                .find(|(s, _, _)| *s == b.reg)
                .map_or(env.statics.1, |(_, a, _)| *a),
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
            BindKind::Sbuf => match inp
                .sbufs
                .iter()
                .find(|s| s.stage == inp.stage && s.slot == b.reg)
            {
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
