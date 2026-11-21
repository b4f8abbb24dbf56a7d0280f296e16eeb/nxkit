//! The bind functions of the command buffer: program, buffers, textures, images, samplers, vertex input, state
//! objects and the descriptor pools.

use nxkit_nvn::ext::{ExtCode, StateKind};
use nxkit_nvn::record::Record;

use super::args::{hi, lo, load_bytes, range, slot, span, stage};
use super::rec::{O_SAMP_BASE, O_TEX_BASE, emit, emit_rec};
use super::seam::{channel_mask_patch, color_patch, depth_stencil_patch, seam};
use crate::nvn::abi::{AbiResult, Obj, guard, log_once, other, table};

/// The most uniform and storage buffers of a stage.
const BUFFERS: u16 = 16;
/// The most vertex streams and the most vertex attributes.
const STREAMS: u32 = 16;
const ATTRIBS: u32 = 32;
/// The most handles of one bind and the slots of a stage bank.
const HANDLES: u32 = 32;
/// The entries of a vertex state chunk: two words of header and up to 26 words of entries.
const ATTRIBS_PER_CHUNK: u32 = 26;
const STREAMS_PER_CHUNK: u32 = 13;

extern "C" fn bind_program(c: u64, program: u64, stages: i32) {
    guard("nvnCommandBufferBindProgram", || {
        if crate::nvn::program::has_pending("nvnCommandBufferBindProgram", program) {
            crate::nvn::program::stages_of("nvnCommandBufferBindProgram", program);
        }
        emit(
            "nvnCommandBufferBindProgram",
            c,
            ExtCode::Program,
            0,
            &[lo(program), hi(program), stages as u32],
        );
        Ok(())
    });
}

fn bind_buffer(
    name: &'static str,
    code: ExtCode,
    c: u64,
    st: i32,
    index: i32,
    addr: u64,
    n: u64,
) -> AbiResult<()> {
    let s = stage(st)?;
    let i = u16::try_from(index)
        .ok()
        .filter(|i| *i < BUFFERS)
        .ok_or_else(|| other(format!("buffer binding {index} is not valid")))?;
    emit(
        name,
        c,
        code,
        s << 8 | i,
        &[lo(addr), hi(addr), lo(n), hi(n)],
    );
    Ok(())
}

fn bind_buffers(
    name: &'static str,
    code: ExtCode,
    c: u64,
    st: i32,
    first: i32,
    n: i32,
    ranges: u64,
) -> AbiResult<()> {
    let (first, n) = span(first, n, u32::from(BUFFERS))?;
    for k in 0..n {
        let (addr, size) = range(ranges, k)?;
        bind_buffer(name, code, c, st, (first + k) as i32, addr, size)?;
    }
    Ok(())
}

extern "C" fn bind_uniform_buffer(c: u64, st: i32, i: i32, addr: u64, n: u64) {
    let name = "nvnCommandBufferBindUniformBuffer";
    guard(name, || {
        bind_buffer(name, ExtCode::UniformBuffer, c, st, i, addr, n)
    });
}

extern "C" fn bind_uniform_buffers(c: u64, st: i32, first: i32, n: i32, ranges: u64) {
    let name = "nvnCommandBufferBindUniformBuffers";
    guard(name, || {
        bind_buffers(name, ExtCode::UniformBuffer, c, st, first, n, ranges)
    });
}

extern "C" fn bind_storage_buffer(c: u64, st: i32, i: i32, addr: u64, n: u64) {
    let name = "nvnCommandBufferBindStorageBuffer";
    guard(name, || {
        bind_buffer(name, ExtCode::StorageBuffer, c, st, i, addr, n)
    });
}

extern "C" fn bind_storage_buffers(c: u64, st: i32, first: i32, n: i32, ranges: u64) {
    let name = "nvnCommandBufferBindStorageBuffers";
    guard(name, || {
        bind_buffers(name, ExtCode::StorageBuffer, c, st, first, n, ranges)
    });
}

/// Which slots a handle bind writes.
#[derive(Clone, Copy)]
enum Kind {
    /// A combined texture and sampler handle.
    Texture,
    /// An image handle.
    Image,
    /// A texture handle without a sampler.
    Separate,
}

/// The id bases of the descriptor pools that the command buffer set.
#[derive(Clone, Copy)]
pub(super) struct Bases {
    pub(super) tex: u32,
    pub(super) samp: u32,
}

pub(super) fn bases(c: u64) -> AbiResult<Bases> {
    Ok(Bases {
        tex: Obj(c).u32(O_TEX_BASE)?,
        samp: Obj(c).u32(O_SAMP_BASE)?,
    })
}

/// Moves the ids of a handle into the descriptor table. The texture id is bits 0 to 19, the sampler id bits 20 to
/// 31. An id of 0 means no descriptor and stays 0.
pub(super) fn adjust(h: u64, b: Bases, tex: bool, samp: bool) -> u64 {
    nxkit_nvn::record::adjust_handle(h, b.tex, b.samp, tex, samp)
}

fn bind_handle(c: u64, kind: Kind, st: u16, index: i32, h: u64, b: Bases) -> AbiResult<()> {
    let slot = slot(st, index)?;
    let name = "nvnCommandBufferBindTexture";
    match kind {
        Kind::Texture => emit_rec(
            name,
            c,
            Record::BindTexture {
                slot,
                handle: adjust(h, b, true, true),
            },
        ),
        Kind::Separate => emit_rec(
            name,
            c,
            Record::BindTexture {
                slot,
                handle: adjust(h, b, true, false),
            },
        ),
        Kind::Image => emit_rec(
            name,
            c,
            Record::BindImage {
                slot,
                handle: adjust(h, b, true, false),
            },
        ),
    }
    Ok(())
}

fn bind_one(c: u64, kind: Kind, st: i32, index: i32, h: u64) -> AbiResult<()> {
    bind_handle(c, kind, stage(st)?, index, h, bases(c)?)
}

fn bind_many(c: u64, kind: Kind, st: i32, first: i32, n: i32, handles: u64) -> AbiResult<()> {
    let s = stage(st)?;
    let (first, n) = span(first, n, HANDLES)?;
    let b = bases(c)?;
    for k in 0..n {
        let h = Obj(handles + u64::from(k) * 8).u64(0)?;
        bind_handle(c, kind, s, (first + k) as i32, h, b)?;
    }
    Ok(())
}

extern "C" fn bind_texture(c: u64, st: i32, i: i32, h: u64) {
    guard("nvnCommandBufferBindTexture", || {
        bind_one(c, Kind::Texture, st, i, h)
    });
}

extern "C" fn bind_textures(c: u64, st: i32, first: i32, n: i32, h: u64) {
    guard("nvnCommandBufferBindTextures", || {
        bind_many(c, Kind::Texture, st, first, n, h)
    });
}

/// `nvnCommandBufferBindTexturesDeferred`: the GPU reads the `n` handles at `h` when the command runs, and the game
/// writes them after recording. The record keeps their address, and the readers of the record take the handles from
/// memory then.
extern "C" fn bind_textures_deferred(c: u64, st: i32, first: i32, n: i32, h: u64) {
    let name = "nvnCommandBufferBindTexturesDeferred";
    guard(name, || {
        let s = stage(st)?;
        let (first, n) = span(first, n, HANDLES)?;
        if n == 0 {
            return Ok(());
        }
        let b = bases(c)?;
        emit_rec(
            name,
            c,
            Record::BindTexturesDeferred {
                first: slot(s, first as i32)?,
                count: n as u8,
                src: h,
                tex_base: b.tex,
                samp_base: b.samp,
            },
        );
        Ok(())
    });
}

extern "C" fn bind_image(c: u64, st: i32, i: i32, h: u64) {
    guard("nvnCommandBufferBindImage", || {
        bind_one(c, Kind::Image, st, i, h)
    });
}

extern "C" fn bind_images(c: u64, st: i32, first: i32, n: i32, h: u64) {
    guard("nvnCommandBufferBindImages", || {
        bind_many(c, Kind::Image, st, first, n, h)
    });
}

extern "C" fn bind_separate_texture(c: u64, st: i32, i: i32, h: u64) {
    guard("nvnCommandBufferBindSeparateTexture", || {
        bind_one(c, Kind::Separate, st, i, h)
    });
}

extern "C" fn bind_separate_textures(c: u64, st: i32, first: i32, n: i32, h: u64) {
    guard("nvnCommandBufferBindSeparateTextures", || {
        bind_many(c, Kind::Separate, st, first, n, h)
    });
}

fn bind_sampler(name: &'static str, c: u64, s: u16, index: u32, h: u64, b: Bases) {
    let id = adjust(h, b, false, true) as u32;
    emit(
        name,
        c,
        ExtCode::SeparateSampler,
        s << 8 | index as u16,
        &[id],
    );
}

extern "C" fn bind_separate_sampler(c: u64, st: i32, i: i32, h: u64) {
    let name = "nvnCommandBufferBindSeparateSampler";
    guard(name, || {
        let (i, _) = span(i, 1, HANDLES)?;
        bind_sampler(name, c, stage(st)?, i, h, bases(c)?);
        Ok(())
    });
}

extern "C" fn bind_separate_samplers(c: u64, st: i32, first: i32, n: i32, handles: u64) {
    let name = "nvnCommandBufferBindSeparateSamplers";
    guard(name, || {
        let s = stage(st)?;
        let (first, n) = span(first, n, HANDLES)?;
        let b = bases(c)?;
        for k in 0..n {
            let h = Obj(handles + u64::from(k) * 8).u64(0)?;
            bind_sampler(name, c, s, first + k, h, b);
        }
        Ok(())
    });
}

fn vertex_buffer(name: &'static str, c: u64, stream: u32, addr: u64, n: u64) {
    emit(
        name,
        c,
        ExtCode::VertexBuffer,
        stream as u16,
        &[lo(addr), hi(addr), lo(n), hi(n)],
    );
}

extern "C" fn bind_vertex_buffer(c: u64, i: i32, addr: u64, n: u64) {
    let name = "nvnCommandBufferBindVertexBuffer";
    guard(name, || {
        let (i, _) = span(i, 1, STREAMS)?;
        vertex_buffer(name, c, i, addr, n);
        Ok(())
    });
}

extern "C" fn bind_vertex_buffers(c: u64, first: i32, n: i32, ranges: u64) {
    let name = "nvnCommandBufferBindVertexBuffers";
    guard(name, || {
        if first >= STREAMS as i32 && n == 1 && super::settings().ignore_null_stream_binds {
            let (addr, size) = range(ranges, 0)?;
            if addr == 0 && size == 0 {
                log_once(
                    name,
                    "a bind of a null range past the last vertex stream is ignored (title need)",
                );
                return Ok(());
            }
        }
        let (first, n) = span(first, n, STREAMS)?;
        for k in 0..n {
            let (addr, size) = range(ranges, k)?;
            vertex_buffer(name, c, first + k, addr, size);
        }
        Ok(())
    });
}

/// Records `n` entries of `words` words each in chunks. The first two words of a record are the first entry and the
/// count of the chunk.
fn vertex_state(
    name: &'static str,
    c: u64,
    arg: u16,
    n: i32,
    st: u64,
    (words, per_chunk, max): (u32, u32, u32),
) -> AbiResult<()> {
    let (_, n) = span(0, n, max)?;
    let mut start = 0;
    while start < n {
        let count = per_chunk.min(n - start);
        let mut w = [0u32; 28];
        w[0] = start;
        w[1] = count;
        let len = (count * words) as usize;
        load_bytes(
            st + u64::from(start * words * 4),
            len * 4,
            &mut w[2..2 + len],
        )?;
        emit(name, c, ExtCode::VertexState, arg, &w[..2 + len]);
        start += count;
    }
    Ok(())
}

extern "C" fn bind_vertex_attrib_state(c: u64, n: i32, st: u64) {
    let name = "nvnCommandBufferBindVertexAttribState";
    guard(name, || {
        vertex_state(name, c, 0, n, st, (1, ATTRIBS_PER_CHUNK, ATTRIBS))
    });
}

extern "C" fn bind_vertex_stream_state(c: u64, n: i32, st: u64) {
    let name = "nvnCommandBufferBindVertexStreamState";
    guard(name, || {
        vertex_state(name, c, 1, n, st, (2, STREAMS_PER_CHUNK, STREAMS))
    });
}

/// Records a state object. `len` is the size of the guest struct in bytes. `patch` gives the part of the draw state
/// that the object decides.
fn bind_state(
    name: &'static str,
    c: u64,
    kind: StateKind,
    st: u64,
    len: usize,
    patch: fn(&[u32]) -> u8,
) {
    guard(name, || {
        let mut w = [0u32; 6];
        let n = len.div_ceil(4);
        load_bytes(st, len, &mut w[..n])?;
        let arg = u16::from(kind as u8) | u16::from(patch(&w[..n])) << 8;
        emit(name, c, ExtCode::State, arg, &w[..n]);
        Ok(())
    });
}

extern "C" fn bind_blend_state(c: u64, st: u64) {
    bind_state(
        "nvnCommandBufferBindBlendState",
        c,
        StateKind::Blend,
        st,
        8,
        |_| 0,
    );
}

extern "C" fn bind_color_state(c: u64, st: u64) {
    bind_state(
        "nvnCommandBufferBindColorState",
        c,
        StateKind::Color,
        st,
        4,
        color_patch,
    );
}

extern "C" fn bind_channel_mask_state(c: u64, st: u64) {
    bind_state(
        "nvnCommandBufferBindChannelMaskState",
        c,
        StateKind::ChannelMask,
        st,
        4,
        channel_mask_patch,
    );
}

extern "C" fn bind_depth_stencil_state(c: u64, st: u64) {
    bind_state(
        "nvnCommandBufferBindDepthStencilState",
        c,
        StateKind::DepthStencil,
        st,
        8,
        depth_stencil_patch,
    );
}

extern "C" fn bind_polygon_state(c: u64, st: u64) {
    bind_state(
        "nvnCommandBufferBindPolygonState",
        c,
        StateKind::Polygon,
        st,
        4,
        |_| 0,
    );
}

extern "C" fn bind_multisample_state(c: u64, st: u64) {
    bind_state(
        "nvnCommandBufferBindMultisampleState",
        c,
        StateKind::Multisample,
        st,
        0x18,
        |_| 0,
    );
}

extern "C" fn set_texture_pool(c: u64, pool: u64) {
    let name = "nvnCommandBufferSetTexturePool";
    guard(name, || {
        let base = seam().texture_pool_base(pool)?;
        Obj(c).set_u32(O_TEX_BASE, base)?;
        emit(
            name,
            c,
            ExtCode::TexturePool,
            0,
            &[lo(pool), hi(pool), base],
        );
        Ok(())
    });
}

extern "C" fn set_sampler_pool(c: u64, pool: u64) {
    let name = "nvnCommandBufferSetSamplerPool";
    guard(name, || {
        let base = seam().sampler_pool_base(pool)?;
        Obj(c).set_u32(O_SAMP_BASE, base)?;
        emit(
            name,
            c,
            ExtCode::SamplerPool,
            0,
            &[lo(pool), hi(pool), base],
        );
        Ok(())
    });
}

pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnCommandBufferBindProgram" => bind_program as extern "C" fn(u64, u64, i32),
        "nvnCommandBufferBindUniformBuffer" => bind_uniform_buffer as extern "C" fn(u64, i32, i32, u64, u64),
        "nvnCommandBufferBindUniformBuffers" => bind_uniform_buffers as extern "C" fn(u64, i32, i32, i32, u64),
        "nvnCommandBufferBindStorageBuffer" => bind_storage_buffer as extern "C" fn(u64, i32, i32, u64, u64),
        "nvnCommandBufferBindStorageBuffers" => bind_storage_buffers as extern "C" fn(u64, i32, i32, i32, u64),
        "nvnCommandBufferBindTexture" => bind_texture as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferBindTextures" => bind_textures as extern "C" fn(u64, i32, i32, i32, u64),
        "nvnCommandBufferBindTexturesDeferred" => bind_textures_deferred as extern "C" fn(u64, i32, i32, i32, u64),
        "nvnCommandBufferBindImage" => bind_image as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferBindImages" => bind_images as extern "C" fn(u64, i32, i32, i32, u64),
        "nvnCommandBufferBindSeparateTexture" => bind_separate_texture as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferBindSeparateTextures" => bind_separate_textures as extern "C" fn(u64, i32, i32, i32, u64),
        "nvnCommandBufferBindSeparateSampler" => bind_separate_sampler as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferBindSeparateSamplers" => bind_separate_samplers as extern "C" fn(u64, i32, i32, i32, u64),
        "nvnCommandBufferBindVertexBuffer" => bind_vertex_buffer as extern "C" fn(u64, i32, u64, u64),
        "nvnCommandBufferBindVertexBuffers" => bind_vertex_buffers as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferBindVertexAttribState" => bind_vertex_attrib_state as extern "C" fn(u64, i32, u64),
        "nvnCommandBufferBindVertexStreamState" => bind_vertex_stream_state as extern "C" fn(u64, i32, u64),
        "nvnCommandBufferBindBlendState" => bind_blend_state as extern "C" fn(u64, u64),
        "nvnCommandBufferBindColorState" => bind_color_state as extern "C" fn(u64, u64),
        "nvnCommandBufferBindChannelMaskState" => bind_channel_mask_state as extern "C" fn(u64, u64),
        "nvnCommandBufferBindDepthStencilState" => bind_depth_stencil_state as extern "C" fn(u64, u64),
        "nvnCommandBufferBindPolygonState" => bind_polygon_state as extern "C" fn(u64, u64),
        "nvnCommandBufferBindMultisampleState" => bind_multisample_state as extern "C" fn(u64, u64),
        "nvnCommandBufferSetTexturePool" => set_texture_pool as extern "C" fn(u64, u64),
        "nvnCommandBufferSetSamplerPool" => set_sampler_pool as extern "C" fn(u64, u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_moves_into_the_pool_of_its_command_buffer() {
        let b = Bases { tex: 100, samp: 7 };
        let h = u64::from(3u32 << 20 | 5);
        assert_eq!(adjust(h, b, true, true), u64::from(10u32 << 20 | 105));
        assert_eq!(adjust(h, b, true, false), u64::from(3u32 << 20 | 105));
        assert_eq!(adjust(0, b, true, true), 0);
    }
}
