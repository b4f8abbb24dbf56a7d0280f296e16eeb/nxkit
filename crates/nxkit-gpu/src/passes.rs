//! Compute, copy and copy-back passes.
//!
//! Dispatches run the translated compute shaders with an argument buffer that carries the dispatch size and the
//! uniform buffer table in its driver block (see `bindless`). Copies are texture to texture on the GPU. A copy
//! between a scaled and an unscaled texture is not encoded. A CPU read of a texture the GPU wrote swizzles the
//! texture back into guest memory (R4).

use nxkit_metal::{CommandBuffer, ComputeCommand, ComputePass, Scratch, TextureDesc, TextureKind, TextureRegion};
use nxkit_nvn::frame::Cmd;
use nxkit_nvn::passes::{Pass, PassKind};
use nxkit_nvn::resource::{Residency, TexId};

use crate::bindless::{ScratchSink, StageInputs, fill_args};
use crate::detail::{CopyDetail, CopyKind, Stage};
use crate::draws::Shared;
use crate::encode::Tables;
use crate::stats::{EncodeStats, Skip};

fn dispatch(sh: &Shared<'_>, cp: &mut ComputePass<'_>, scratch: &Scratch, n: u32) -> Result<(), Skip> {
    // A dispatch of a run that a native plan replaces: the plan is encoded after the pass of the last one.
    if sh.native.is_some_and(|p| p.skips(n)) {
        return Ok(());
    }
    let dd = sh.detail.dispatches.get(n as usize).ok_or(Skip::Detail)?;
    if crate::diag::skipped_program(&dd.program) {
        return Err(Skip::Encode);
    }
    let p = sh.programs.stage(&dd.program, false)?;
    let pipeline = sh.pipelines.compute(&dd.program, &p)?;
    let inputs = StageInputs {
        stage: Stage::Compute,
        ubos: &dd.ubos,
        sbufs: &dd.sbufs,
        textures: &dd.textures,
        images: &dd.images,
        base_instance: 0,
        groups: Some(dd.groups),
        push: dd.push.as_deref(),
    };
    let args = fill_args(&p, &inputs, &sh.env, &mut ScratchSink::new(scratch, sh.uploaded, None).with_preloaded(sh.preloaded))?;
    let enc = |cp: &mut ComputePass<'_>, c| cp.encode(&c).map_err(|_| Skip::Encode);
    enc(cp, ComputeCommand::SetPipeline(pipeline))?;
    enc(cp, ComputeCommand::SetBuffer { index: 0, gpu_addr: args })?;
    enc(cp, ComputeCommand::DispatchThreadgroups { groups: dd.groups, threads: p.threadgroup })
}

fn scaled(v: [u32; 3], s: f32) -> [u32; 3] {
    [(v[0] as f32 * s).round() as u32, (v[1] as f32 * s).round() as u32, v[2]]
}

/// Prints a refused copy under the `copy` category (at most 400 lines), keeps it as an example of the health report
/// and gives back `Skip::Copy`.
fn copy_skip(what: std::fmt::Arguments<'_>) -> Skip {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    nxkit_nvn::diag::example(nxkit_nvn::diag::Reason::SkipCopy, what);
    if crate::diag("copy") && crate::diag::first(&SHOWN, 400) {
        eprintln!("DIAG copy {what}");
    }
    Skip::Copy
}

/// The slice range and the box of a copy side, for a texture of kind `d`. A 3D texture takes the third value of the
/// origin and the size as z and depth. The other kinds take them as the first layer and the layer count.
fn side(d: &TextureDesc, level: u32, layer: u32, origin: [u32; 3], size: [u32; 3], factor: f32) -> (TextureRegion, u32) {
    let (origin, size) = (scaled(origin, factor), scaled(size, factor));
    if d.kind == TextureKind::D3 {
        (TextureRegion { level, slice: 0, origin, size }, 1)
    } else {
        let region = TextureRegion { level, slice: layer.saturating_add(origin[2]), origin: [origin[0], origin[1], 0], size: [size[0], size[1], 1] };
        (region, size[2])
    }
}

/// The bytes of one row of blocks and of the whole image of a box in a tightly packed buffer layout.
fn packed(d: &TextureDesc, size: [u32; 3]) -> (u64, u64) {
    let (bw, bh) = d.format.block_dim();
    let row = u64::from(size[0].div_ceil(bw)) * u64::from(d.format.block_bytes());
    (row, row * u64::from(size[1].div_ceil(bh)))
}

/// True when `count` slices of the box are inside the level and slice range of the texture, and a box of a compressed
/// texture that is cut by a block ends at the edge of the level.
fn box_fits(d: &TextureDesc, region: &TextureRegion, count: u32) -> bool {
    let (bw, bh) = d.format.block_dim();
    let extent = d.level_extent(region.level);
    let ends = |i: usize, b: u32| region.origin[i].saturating_add(region.size[i]) == extent[i] || region.size[i] % b == 0;
    !region.size.contains(&0)
        && count != 0
        && region.level < d.mip_levels
        && region.slice.saturating_add(count) <= d.slice_count()
        && (0..3).all(|i| region.origin[i].saturating_add(region.size[i]) <= extent[i])
        && ends(0, bw)
        && ends(1, bh)
}

/// The bytes of a clear block: one run of rows of the clear colour, at most this many.
const CLEAR_BLOCK_BYTES: usize = 64 * 1024;

/// Encodes `nvnCommandBufferClearTexture` as blits of the clear colour. The colour is packed into one texel, repeated
/// into a block of rows in the scratch buffer, and the box is filled block by block (and slice by slice) from it.
fn clear_texture(sh: &Shared<'_>, cp: &mut ComputePass<'_>, scratch: &Scratch, cmd: Cmd, c: &CopyDetail) -> Result<(), Skip> {
    let Cmd::Copy { src: None, dst: Some(tex) } = cmd else {
        return Err(copy_skip(format_args!("clear does not match the command {cmd:?}")));
    };
    let res = sh.textures.get(&tex).ok_or(Skip::Texture)?;
    let d = res.desc;
    let texel = crate::clear::texel(d.format, c.clear_kind, c.clear, c.channel_mask)
        .map_err(|why| copy_skip(format_args!("clear of {tex:?} {:?}: {why} ({c:?})", d.format)))?;
    let factor = if res.scaled { sh.scale } else { 1.0 };
    let (region, count) = side(&d, c.dst_level, c.dst_layer, c.dst_origin, c.size, factor);
    if !box_fits(&d, &region, count) {
        return Err(copy_skip(format_args!("clear box {c:?} is outside {tex:?} {}x{} level {}", d.width, d.height, region.level)));
    }
    let (width, height) = (region.size[0] as usize, region.size[1] as usize);
    let row = width * texel.len();
    let rows = (CLEAR_BLOCK_BYTES / row).clamp(1, height);
    let block: Vec<u8> = texel.iter().copied().cycle().take(row * rows).collect();
    let address = scratch.push(&block).map_err(|_| Skip::Buffer)?;
    let is_3d = d.kind == TextureKind::D3;
    let slices = if is_3d { region.size[2] } else { count };
    for i in 0..slices {
        for y in (0..height).step_by(rows) {
            let mut part = TextureRegion { size: [region.size[0], rows.min(height - y) as u32, 1], ..region };
            part.origin[1] += y as u32;
            if is_3d {
                part.origin[2] += i;
            } else {
                part.slice += i;
            }
            cp.copy_scratch_to_texture(scratch, address, row as u32, &res.tex, &part)
                .map_err(|e| copy_skip(format_args!("clear of {tex:?} {:?}: {e}", d.format)))?;
        }
    }
    Ok(())
}

/// Encodes a copy between guest memory and a texture. A texture to buffer copy writes guest memory on the GPU.
///
/// Nothing tells the texture table about that write: a texture whose block-linear storage overlaps the destination
/// stays `InSync` or `GpuNewer`, so a later sample of it does not see the new bytes. The CPU reads the bytes as they
/// are in guest memory once the frame is done.
fn copy_buffer(sh: &Shared<'_>, cp: &mut ComputePass<'_>, cmd: Cmd, c: &CopyDetail) -> Result<(), Skip> {
    let to_texture = c.kind == CopyKind::BufferToTexture;
    let tex = match cmd {
        Cmd::Copy { src: None, dst: Some(t) } if to_texture => t,
        Cmd::Copy { src: Some(t), dst: None } if !to_texture => t,
        _ => return Err(copy_skip(format_args!("buffer copy {:?} does not match the command {cmd:?}", c.kind))),
    };
    let res = sh.textures.get(&tex).ok_or(Skip::Texture)?;
    let d = res.desc;
    if res.scaled {
        return Err(copy_skip(format_args!("buffer copy of the scaled texture {tex:?} {}x{}: guest bytes are unscaled", d.width, d.height)));
    }
    if d.format.is_depth() {
        return Err(copy_skip(format_args!("buffer copy of the depth texture {tex:?} {:?}", d.format)));
    }
    let (level, layer, origin) = if to_texture { (c.dst_level, c.dst_layer, c.dst_origin) } else { (c.src_level, c.src_layer, c.src_origin) };
    let (region, count) = side(&d, level, layer, origin, c.size, 1.0);
    if !box_fits(&d, &region, count) {
        return Err(copy_skip(format_args!("buffer copy box {c:?} is outside {tex:?} {}x{} level {}", d.width, d.height, region.level)));
    }
    let (row, image) = packed(&d, region.size);
    let bytes_per_row = if c.row_stride != 0 { c.row_stride } else { row };
    let bytes_per_image = if c.image_stride != 0 { c.image_stride } else { bytes_per_row * (image / row.max(1)) };
    let (Ok(bytes_per_row), Ok(bytes_per_image)) = (u32::try_from(bytes_per_row), u32::try_from(bytes_per_image)) else {
        return Err(copy_skip(format_args!("buffer copy strides {c:?} are too large")));
    };
    let images = u64::from(count.max(region.size[2]));
    let len = u64::from(bytes_per_image) * (images - 1) + u64::from(bytes_per_row) * image.div_ceil(row.max(1));
    let Some(offset) = sh.guest_offset(c.buffer, len) else {
        return Err(copy_skip(format_args!("buffer address {:#x} + {len} is outside guest memory", c.buffer)));
    };
    for i in 0..count {
        let region = TextureRegion { slice: region.slice + i, ..region };
        let offset = offset + u64::from(bytes_per_image) * u64::from(i);
        let command = if to_texture {
            ComputeCommand::CopyGuestToTexture { guest: sh.guest.clone(), offset, bytes_per_row, bytes_per_image, texture: res.tex.clone(), region }
        } else {
            ComputeCommand::CopyTextureRegionToGuest { texture: res.tex.clone(), region, guest: sh.guest.clone(), offset, bytes_per_row, bytes_per_image }
        };
        if let Err(e) = cp.encode(&command) {
            return Err(copy_skip(format_args!("buffer copy {c:?} to {tex:?} {:?}: {e}", d.format)));
        }
    }
    Ok(())
}

fn copy(sh: &Shared<'_>, cp: &mut ComputePass<'_>, scratch: &Scratch, cmd: Cmd, n: u32) -> Result<(), Skip> {
    let entry = sh.detail.copies.get(n as usize);
    static TCOPY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if crate::diag("tcopy") && crate::diag::tex_filtered() {
        let hit = |t: Option<TexId>| {
            t.and_then(|t| sh.textures.get(&t)).is_some_and(|r| {
                crate::diag::tex_match(r.desc.format, r.desc.width, r.desc.height, r.desc.array_layers)
            })
        };
        if let Cmd::Copy { src, dst } = cmd {
            if (hit(src) || hit(dst)) && crate::diag::first(&TCOPY, 300) {
                let f = |t: Option<TexId>| t.and_then(|t| sh.textures.get(&t).map(|r| (t, r.desc.width, r.desc.format)));
                eprintln!("DIAG tcopy src {:?} dst {:?} detail {:?}", f(src), f(dst), entry);
            }
        }
    }
    if let Some(c) = entry.filter(|c| matches!(c.kind, CopyKind::BufferToTexture | CopyKind::TextureToBuffer)) {
        return copy_buffer(sh, cp, cmd, c);
    }
    if let Some(c) = entry.filter(|c| c.kind == CopyKind::Clear) {
        return clear_texture(sh, cp, scratch, cmd, c);
    }
    let (src, dst) = match cmd {
        Cmd::Copy { src: Some(s), dst: Some(d) } | Cmd::Downsample { src: s, dst: d } => (s, d),
        _ => {
            let f = |t: Option<TexId>| t.and_then(|t| sh.textures.get(&t).map(|r| (t, r.desc.width, r.desc.height, r.desc.format)));
            if let Cmd::Copy { src, dst } = cmd {
                return Err(copy_skip(format_args!(
                    "no texture to texture copy: src {:?} dst {:?} detail {entry:?}",
                    f(src),
                    f(dst),
                )));
            }
            return Err(Skip::Copy);
        }
    };
    if crate::watched(src) || crate::watched(dst) {
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 40 {
            let f = |t: &TexId| sh.textures.get(t).map(|r| (r.desc.width, r.desc.height, r.desc.format, r.scaled));
            eprintln!("DIAG watch copy {src:?} {:?} -> {dst:?} {:?} detail {entry:?}", f(&src), f(&dst));
        }
    }
    let (rs, rd) = (sh.textures.get(&src).ok_or(Skip::Texture)?, sh.textures.get(&dst).ok_or(Skip::Texture)?);
    if rs.scaled != rd.scaled || rs.desc.format != rd.desc.format {
        return Err(copy_skip(format_args!("format {src:?} {:?} {}x{} -> {dst:?} {:?} {}x{}", rs.desc.format, rs.desc.width, rs.desc.height, rd.desc.format, rd.desc.width, rd.desc.height)));
    }
    let factor = if rs.scaled { sh.scale } else { 1.0 };
    let fits = |o: [u32; 3], s: [u32; 3], e: [u32; 3]| (0..3).all(|i| o[i].saturating_add(s[i]) <= e[i]);
    let (src_box, dst_box, count) = match entry.filter(|c| c.kind == CopyKind::TextureToTexture) {
        Some(c) => {
            if c.size != c.dst_size {
                return Err(copy_skip(format_args!("scaled copy {src:?} {:?} -> {dst:?} {:?}", c.size, c.dst_size)));
            }
            let (from, count) = side(&rs.desc, c.src_level, c.src_layer, c.src_origin, c.size, factor);
            let (to, dst_count) = side(&rd.desc, c.dst_level, c.dst_layer, c.dst_origin, c.dst_size, factor);
            if count != dst_count {
                return Err(copy_skip(format_args!("copy {src:?} -> {dst:?} has {count} source and {dst_count} destination layers")));
            }
            (from, to, count)
        }
        None if rs.desc.level_extent(0) == rd.desc.level_extent(0) => {
            let whole = TextureRegion::whole(0, 0, rs.desc.level_extent(0));
            (whole, whole, 1)
        }
        None => return Err(copy_skip(format_args!("size {src:?} {}x{} -> {dst:?} {}x{} {:?}", rs.desc.width, rs.desc.height, rd.desc.width, rd.desc.height, rd.desc.format))),
    };
    if src_box.level >= rs.desc.mip_levels
        || dst_box.level >= rd.desc.mip_levels
        || src_box.slice.saturating_add(count) > rs.desc.slice_count()
        || dst_box.slice.saturating_add(count) > rd.desc.slice_count()
        || !fits(src_box.origin, src_box.size, rs.desc.level_extent(src_box.level))
        || !fits(dst_box.origin, src_box.size, rd.desc.level_extent(dst_box.level))
    {
        return Err(Skip::Copy);
    }
    for i in 0..count {
        cp.encode(&ComputeCommand::CopyTexture {
            src: rs.tex.clone(),
            dst: rd.tex.clone(),
            src_region: TextureRegion { slice: src_box.slice + i, ..src_box },
            dst_level: dst_box.level,
            dst_slice: dst_box.slice + i,
            dst_origin: dst_box.origin,
        })
        .map_err(|_| Skip::Encode)?;
    }
    Ok(())
}

/// Encodes a compute pass (dispatches) or a copy pass (copies and downsamples).
pub(crate) fn compute_pass(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    tables: &Tables,
    scratch: &Scratch,
    index: usize,
    pass: &Pass,
    stats: &mut EncodeStats,
) {
    let Ok(mut cp) = cb.begin_compute_pass(&tables.vertex) else {
        stats.skipped[Skip::Encode as usize] += pass.cmds.len() as u32;
        return;
    };
    stats.compute_passes += 1;
    if crate::diag::gpu_labels() {
        cp.set_label(&format!("c{index} n{}", pass.cmds.len()));
    }
    if let Some(dep) = sh.barriers.before.get(index).copied().flatten() {
        if cp.encode(&ComputeCommand::Depend(dep)).is_err() {
            stats.skip(Skip::Encode);
        }
    }
    let mut native_here = None;
    for i in pass.cmds.clone() {
        let Some(cmd) = sh.frame.cmds.get(i as usize).copied() else { continue };
        let ord = sh.ordinals.get(i as usize).copied().unwrap_or_default();
        if let (PassKind::Compute, Cmd::Dispatch { .. }, Some(p)) = (pass.kind, cmd, sh.native)
            && p.is_anchor(ord.dispatch)
        {
            native_here = Some(p);
        }
        let r = match (pass.kind, cmd) {
            (PassKind::Compute, Cmd::Dispatch { .. }) => dispatch(sh, &mut cp, scratch, ord.dispatch),
            (PassKind::Copy, Cmd::Copy { .. } | Cmd::Downsample { .. }) => copy(sh, &mut cp, scratch, cmd, ord.copy),
            _ => Ok(()),
        };
        if crate::diag("comp") && crate::diag::sampled() {
            if let (PassKind::Compute, Some(dd)) = (pass.kind, sh.detail.dispatches.get(ord.dispatch as usize)) {
                eprintln!(
                    "DIAG comp pass {index} cmd {i} program {:?} groups {:?} textures {} sbufs {} result {:?}",
                    dd.program,
                    dd.groups,
                    dd.textures.len(),
                    dd.sbufs.len(),
                    r
                );
            }
        }
        if let Err(why) = r {
            stats.skip(why);
        }
    }
    cp.end();
    if let Some(p) = native_here {
        crate::native::encode(sh, cb, scratch, index, p, stats);
    }
}

/// Encodes the copy-back of the textures that a CPU read pass names. A texture is copied when the table says the GPU
/// wrote it earlier or when an earlier pass of this frame writes it. Returns the textures that were copied.
pub(crate) fn cpu_read(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    index: usize,
    pass: &Pass,
    stats: &mut EncodeStats,
) -> Vec<TexId> {
    let mut done = Vec::new();
    for i in pass.cmds.clone() {
        let Some(Cmd::CpuRead(id)) = sh.frame.cmds.get(i as usize).copied() else { continue };
        let (Ok(rec), Some(res)) = (sh.table.record(id), sh.textures.get(&id)) else { continue };
        let written_here = sh.first_write.get(&id).is_some_and(|w| *w < index);
        if !(rec.residency == Residency::GpuNewer || written_here) || done.contains(&id) {
            continue;
        }
        match sh.transfer.copy_back(cb, scratch, sh.guest, rec, res) {
            Ok(()) => {
                stats.copybacks += 1;
                stats.compute_passes += 1;
                done.push(id);
            }
            Err(why) => stats.skip(why),
        }
    }
    done
}
