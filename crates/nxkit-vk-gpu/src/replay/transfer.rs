//! Copies, fills, updates and dispatches in compute segments, and the clears of whole images (the reference layer's
//! `C_COPYB`, `C_COPYB2I`, `C_COPYI`, `C_COPYI2B`, `C_DISPATCH` and the clears of images).

use std::sync::Arc;

use nxkit_metal::{
    CommandBuffer, ComputeCommand, ComputePass, GuestMemory, Texture, TextureKind, TextureRegion,
};
use nxkit_vk::cmd::{BufferImageCopy, Cmd};
use nxkit_vk::desc::{BufferRef, ImageRef};

use super::{Replay, State};
use crate::bind::{self, Bound};

/// Whether a command runs in a compute segment.
pub(super) fn is_compute(c: &Cmd) -> bool {
    matches!(
        c,
        Cmd::CopyBuffer { .. }
            | Cmd::CopyBufferToImage { .. }
            | Cmd::CopyImageToBuffer { .. }
            | Cmd::CopyImage { .. }
            | Cmd::BlitImage { .. }
            | Cmd::FillBuffer { .. }
            | Cmd::UpdateBuffer { .. }
            | Cmd::Dispatch(_)
            | Cmd::DispatchIndirect(_)
    )
}

/// Runs a compute segment from `cmds[i]`: every compute command, and the state commands between them, up to the first
/// command that needs a render pass. Gives the index after it.
pub(super) fn segment(
    r: &mut Replay<'_>,
    st: &mut State,
    cb: &mut CommandBuffer,
    cmds: &[&Cmd],
    i: usize,
) -> usize {
    let mut pass = match cb.begin_compute_pass(&r.tables.compute) {
        Ok(p) => p,
        Err(e) => {
            r.log.once(
                "compute pass",
                format_args!("[RED] a compute pass cannot begin: {e}"),
            );
            return i + 1;
        }
    };
    let _ = pass.encode(&ComputeCommand::WaitForEarlierPasses);
    let mut j = i;
    let mut first = true;
    while let Some(&c) = cmds.get(j) {
        if is_compute(c) {
            if !first {
                let _ = pass.encode(&ComputeCommand::Barrier);
            }
            first = false;
            if let Err(e) = one(r, st, &mut pass, c) {
                r.log.once(
                    format!("{} {e}", c.name()),
                    format_args!("[RED] {} is skipped: {e}", c.name()),
                );
            }
        } else if matches!(c, Cmd::PipelineBarrier) || st.apply(c, r.sets) {
            // a barrier is kept by the barrier before every command
        } else {
            break;
        }
        j += 1;
    }
    pass.end();
    j
}

fn memory(r: &Replay<'_>, b: &BufferRef) -> Result<Arc<GuestMemory>, String> {
    r.objects
        .memories
        .get(&b.memory)
        .cloned()
        .ok_or_else(|| "a buffer has no memory".to_string())
}

pub(super) fn texture(r: &Replay<'_>, i: &ImageRef) -> Result<Texture, String> {
    r.objects
        .texture(i.texture)
        .cloned()
        .ok_or_else(|| "an image has no texture".to_string())
}

/// The layout of a region in the buffer: bytes per row and bytes per image, from the format's block size.
fn buffer_layout(format: u32, c: &BufferImageCopy) -> Result<(u32, u32), String> {
    let f = nxkit_vk::format::lookup(format).ok_or("the format of the image is unknown")?;
    if f.has_depth() || f.has_stencil() {
        return Err("copies of depth and stencil images are not implemented".into());
    }
    let (bw, bh) = (u32::from(f.block_w.max(1)), u32::from(f.block_h.max(1)));
    let rl = if c.row_length == 0 {
        c.extent[0]
    } else {
        c.row_length
    };
    let ih = if c.image_height == 0 {
        c.extent[1]
    } else {
        c.image_height
    };
    let bpr = rl.div_ceil(bw) * u32::from(f.bytes);
    Ok((bpr, bpr * ih.div_ceil(bh)))
}

/// The regions of one buffer-image copy: per layer (one for a 3D image), its buffer offset and its box.
fn regions(t: &Texture, c: &BufferImageCopy, bpi: u32) -> Vec<(u64, TextureRegion)> {
    let is3d = t.desc().kind == TextureKind::D3;
    let layers = if is3d { 1 } else { c.sub.layers.max(1) };
    let [x, y, z] = c.offset.map(|v| v.max(0) as u32);
    (0..layers)
        .map(|l| {
            let region = TextureRegion {
                level: c.sub.level,
                slice: if is3d { 0 } else { c.sub.base_layer + l },
                origin: [x, y, if is3d { z } else { 0 }],
                size: [c.extent[0], c.extent[1], if is3d { c.extent[2] } else { 1 }],
            };
            (c.buffer_offset + u64::from(l) * u64::from(bpi), region)
        })
        .collect()
}

fn e(r: nxkit_metal::Result<()>) -> Result<(), String> {
    r.map_err(|e| e.to_string())
}

/// Encodes one compute command.
fn one(r: &mut Replay<'_>, st: &State, pass: &mut ComputePass<'_>, c: &Cmd) -> Result<(), String> {
    match c {
        Cmd::CopyBuffer { src, dst, regions } => {
            let (s, d) = (memory(r, src)?, memory(r, dst)?);
            for g in regions {
                e(pass.encode(&ComputeCommand::CopyGuestToGuest {
                    src: s.clone(),
                    src_offset: src.offset + g.src_offset,
                    dst: d.clone(),
                    dst_offset: dst.offset + g.dst_offset,
                    size: g.size,
                }))?;
                r.stats.copies += 1;
            }
        }
        Cmd::CopyBufferToImage {
            src,
            dst,
            regions: rs,
        } => {
            let (guest, tex) = (memory(r, src)?, texture(r, dst)?);
            for c in rs {
                let (bpr, bpi) = buffer_layout(dst.format, c)?;
                for (off, region) in regions(&tex, c, bpi) {
                    e(pass.encode(&ComputeCommand::CopyGuestToTexture {
                        guest: guest.clone(),
                        offset: src.offset + off,
                        bytes_per_row: bpr,
                        bytes_per_image: bpi,
                        texture: tex.clone(),
                        region,
                    }))?;
                }
                r.stats.copies += 1;
            }
        }
        Cmd::CopyImageToBuffer {
            src,
            dst,
            regions: rs,
        } => {
            let (tex, guest) = (texture(r, src)?, memory(r, dst)?);
            for c in rs {
                let (bpr, bpi) = buffer_layout(src.format, c)?;
                for (off, region) in regions(&tex, c, bpi) {
                    e(pass.encode(&ComputeCommand::CopyTextureRegionToGuest {
                        texture: tex.clone(),
                        region,
                        guest: guest.clone(),
                        offset: dst.offset + off,
                        bytes_per_row: bpr,
                        bytes_per_image: bpi,
                    }))?;
                }
                r.stats.copies += 1;
            }
        }
        Cmd::CopyImage {
            src,
            dst,
            regions: rs,
        } => {
            let (a, b) = (texture(r, src)?, texture(r, dst)?);
            for g in rs {
                copy_image(pass, &a, &b, g)?;
                r.stats.copies += 1;
            }
        }
        Cmd::BlitImage {
            src,
            dst,
            regions: rs,
            ..
        } => {
            let (a, b) = (texture(r, src)?, texture(r, dst)?);
            for g in rs {
                let [s0, s1] = g.src_offsets;
                let [d0, d1] = g.dst_offsets;
                let size: Vec<i32> = (0..3).map(|k| s1[k] - s0[k]).collect();
                let same = (0..3).all(|k| d1[k] - d0[k] == size[k] && size[k] > 0);
                if src.format != dst.format || !same {
                    return Err(
                        "a scaling or format-changing vkCmdBlitImage is not implemented".into(),
                    );
                }
                let copy = nxkit_vk::cmd::ImageCopy {
                    src_sub: g.src_sub,
                    src_offset: s0,
                    dst_sub: g.dst_sub,
                    dst_offset: d0,
                    extent: [size[0] as u32, size[1] as u32, size[2] as u32],
                };
                copy_image(pass, &a, &b, &copy)?;
                r.stats.copies += 1;
            }
        }
        Cmd::FillBuffer { dst, data } => {
            let len = dst.size & !3;
            let bytes: Vec<u8> = (0..len / 4).flat_map(|_| data.to_le_bytes()).collect();
            update(r, pass, dst, &bytes)?;
        }
        Cmd::UpdateBuffer { dst, data } => update(r, pass, dst, data)?,
        Cmd::Dispatch(groups) => {
            if groups.contains(&0) {
                return Ok(());
            }
            let threads = dispatch_setup(r, st, pass)?;
            e(pass.encode(&ComputeCommand::DispatchThreadgroups {
                groups: *groups,
                threads,
            }))?;
            r.stats.dispatches += 1;
        }
        Cmd::DispatchIndirect(b) => {
            let threads = dispatch_setup(r, st, pass)?;
            let gpu_addr = r
                .objects
                .address(b.memory, b.offset)
                .ok_or("the indirect buffer has no memory")?;
            e(pass.encode(&ComputeCommand::DispatchThreadgroupsIndirect { gpu_addr, threads }))?;
            r.stats.dispatches += 1;
        }
        _ => {}
    }
    Ok(())
}

/// Writes bytes into a buffer in the order of the pass, through the scratch buffer.
fn update(
    r: &mut Replay<'_>,
    pass: &mut ComputePass<'_>,
    dst: &BufferRef,
    bytes: &[u8],
) -> Result<(), String> {
    if bytes.is_empty() {
        return Ok(());
    }
    let guest = memory(r, dst)?;
    let address = r.scratch.push(bytes).map_err(|e| e.to_string())?;
    e(pass.copy_scratch_to_guest(&r.scratch, address, &guest, dst.offset, bytes.len() as u64))?;
    r.stats.copies += 1;
    Ok(())
}

/// Copies one region between images, layer by layer (one slice for a 3D image).
fn copy_image(
    pass: &mut ComputePass<'_>,
    a: &Texture,
    b: &Texture,
    g: &nxkit_vk::cmd::ImageCopy,
) -> Result<(), String> {
    let (a3, b3) = (
        a.desc().kind == TextureKind::D3,
        b.desc().kind == TextureKind::D3,
    );
    let layers = if a3 { 1 } else { g.src_sub.layers.max(1) };
    let p = |v: [i32; 3], is3d: bool| {
        [
            v[0].max(0) as u32,
            v[1].max(0) as u32,
            if is3d { v[2].max(0) as u32 } else { 0 },
        ]
    };
    for l in 0..layers {
        e(pass.encode(&ComputeCommand::CopyTexture {
            src: a.clone(),
            dst: b.clone(),
            src_region: TextureRegion {
                level: g.src_sub.level,
                slice: if a3 { 0 } else { g.src_sub.base_layer + l },
                origin: p(g.src_offset, a3),
                size: [g.extent[0], g.extent[1], if a3 { g.extent[2] } else { 1 }],
            },
            dst_level: g.dst_sub.level,
            dst_slice: if b3 { 0 } else { g.dst_sub.base_layer + l },
            dst_origin: p(g.dst_offset, b3),
        }))?;
    }
    Ok(())
}

/// Binds the compute pipeline, its resources and its push constants. Gives the threadgroup size.
fn dispatch_setup(
    r: &Replay<'_>,
    st: &State,
    pass: &mut ComputePass<'_>,
) -> Result<[u32; 3], String> {
    let id = st.compute.ok_or("no compute pipeline is bound")?;
    let p = r
        .objects
        .compute(id)
        .ok_or_else(|| format!("compute pipeline {:#x} was not built", id.0))?;
    e(pass.encode(&ComputeCommand::SetPipeline(p.pipeline.clone())))?;
    let (mut out, mut problems) = (Vec::new(), Vec::new());
    bind::resolve(
        &p.resources,
        &st.sets[1],
        |h| r.sets.get(&h),
        &mut out,
        &mut problems,
    );
    for b in out {
        let cmd = match b {
            Bound::Buffer { slot, at } => ComputeCommand::SetBuffer {
                index: slot,
                gpu_addr: r
                    .objects
                    .address(at.memory, at.offset)
                    .ok_or("a buffer has no memory")?,
            },
            Bound::Texture { slot, id } => ComputeCommand::SetTexture {
                index: slot,
                texture: r
                    .objects
                    .texture(id)
                    .cloned()
                    .ok_or("a texture was destroyed")?,
            },
            Bound::Sampler { slot, id } => ComputeCommand::SetSampler {
                index: slot,
                sampler: r
                    .objects
                    .samplers
                    .get(&id)
                    .cloned()
                    .ok_or("a sampler was destroyed")?,
            },
        };
        e(pass.encode(&cmd))?;
    }
    if let Some(slot) = bind::push_slot(&p.resources) {
        let data = st.push[2]
            .get(..bind::push_len(p.push_size))
            .unwrap_or_default();
        let gpu_addr = r.scratch.push(data).map_err(|e| e.to_string())?;
        e(pass.encode(&ComputeCommand::SetBuffer {
            index: slot,
            gpu_addr,
        }))?;
    }
    if let Some(p) = problems.first() {
        r.log
            .once(format!("dispatch {p}"), format_args!("[RED] dispatch: {p}"));
    }
    Ok(p.threads)
}
