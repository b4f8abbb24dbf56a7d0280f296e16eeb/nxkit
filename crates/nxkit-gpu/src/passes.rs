//! Compute, copy and copy-back passes.
//!
//! Dispatches run the translated compute shaders with an argument buffer that carries the dispatch size and the
//! uniform buffer table in its driver block (see `bindless`). Copies are texture to texture on the GPU. A copy
//! between a scaled and an unscaled texture is not encoded. A CPU read of a texture the GPU wrote swizzles the
//! texture back into guest memory (R4).

use nxkit_metal::{CommandBuffer, ComputeCommand, ComputePass, Scratch, TextureRegion};
use nxkit_nvn::frame::Cmd;
use nxkit_nvn::passes::{Pass, PassKind};
use nxkit_nvn::resource::{Residency, TexId};

use crate::bindless::{ScratchSink, StageInputs, fill_args};
use crate::detail::Stage;
use crate::draws::Shared;
use crate::encode::Tables;
use crate::stats::{EncodeStats, Skip};

fn dispatch(sh: &Shared<'_>, cp: &mut ComputePass<'_>, scratch: &Scratch, n: u32) -> Result<(), Skip> {
    let dd = sh.detail.dispatches.get(n as usize).ok_or(Skip::Detail)?;
    let p = sh.programs.stage(&dd.program, false)?;
    let pipeline = sh.pipelines.compute(&dd.program, &p)?;
    let inputs = StageInputs {
        stage: Stage::Compute,
        ubos: &dd.ubos,
        textures: &dd.textures,
        base_instance: 0,
        groups: Some(dd.groups),
    };
    let args = fill_args(&p, &inputs, &sh.env, &mut ScratchSink(scratch))?;
    let enc = |cp: &mut ComputePass<'_>, c| cp.encode(&c).map_err(|_| Skip::Encode);
    enc(cp, ComputeCommand::SetPipeline(pipeline))?;
    enc(cp, ComputeCommand::SetBuffer { index: 0, gpu_addr: args })?;
    enc(cp, ComputeCommand::DispatchThreadgroups { groups: dd.groups, threads: p.threadgroup })
}

fn scaled(v: [u32; 3], s: f32) -> [u32; 3] {
    [(v[0] as f32 * s).round() as u32, (v[1] as f32 * s).round() as u32, v[2]]
}

fn copy(sh: &Shared<'_>, cp: &mut ComputePass<'_>, cmd: Cmd, n: u32) -> Result<(), Skip> {
    let (src, dst) = match cmd {
        Cmd::Copy { src: Some(s), dst: Some(d) } | Cmd::Downsample { src: s, dst: d } => (s, d),
        _ => return Err(Skip::Copy),
    };
    let (rs, rd) = (sh.textures.get(&src).ok_or(Skip::Texture)?, sh.textures.get(&dst).ok_or(Skip::Texture)?);
    if rs.scaled != rd.scaled || rs.desc.format != rd.desc.format {
        return Err(Skip::Copy);
    }
    let factor = if rs.scaled { sh.scale } else { 1.0 };
    let region = match sh.detail.copies.get(n as usize) {
        Some(c) => {
            let size = scaled(c.size, factor);
            let region = TextureRegion { level: c.src_level, slice: c.src_layer, origin: scaled(c.src_origin, factor), size };
            (region, c.dst_level, c.dst_layer, scaled(c.dst_origin, factor))
        }
        None if rs.desc.level_extent(0) == rd.desc.level_extent(0) => {
            (TextureRegion::whole(0, 0, rs.desc.level_extent(0)), 0, 0, [0; 3])
        }
        None => return Err(Skip::Copy),
    };
    let (src_region, dst_level, dst_slice, dst_origin) = region;
    let fits = |o: [u32; 3], s: [u32; 3], e: [u32; 3]| (0..3).all(|i| o[i] + s[i] <= e[i]);
    if !fits(src_region.origin, src_region.size, rs.desc.level_extent(src_region.level))
        || !fits(dst_origin, src_region.size, rd.desc.level_extent(dst_level))
    {
        return Err(Skip::Copy);
    }
    cp.encode(&ComputeCommand::CopyTexture {
        src: rs.tex.clone(),
        dst: rd.tex.clone(),
        src_region,
        dst_level,
        dst_slice,
        dst_origin,
    })
    .map_err(|_| Skip::Encode)
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
    if let Some(dep) = sh.barriers.before.get(index).copied().flatten() {
        if cp.encode(&ComputeCommand::Depend(dep)).is_err() {
            stats.skip(Skip::Encode);
        }
    }
    for i in pass.cmds.clone() {
        let Some(cmd) = sh.frame.cmds.get(i as usize).copied() else { continue };
        let ord = sh.ordinals.get(i as usize).copied().unwrap_or_default();
        let r = match (pass.kind, cmd) {
            (PassKind::Compute, Cmd::Dispatch { .. }) => dispatch(sh, &mut cp, scratch, ord.dispatch),
            (PassKind::Copy, Cmd::Copy { .. } | Cmd::Downsample { .. }) => copy(sh, &mut cp, cmd, ord.copy),
            _ => Ok(()),
        };
        if let Err(why) = r {
            stats.skip(why);
        }
    }
    cp.end();
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
        let (Ok(rec), Some(res)) = (sh.table.get(id), sh.textures.get(&id)) else { continue };
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
