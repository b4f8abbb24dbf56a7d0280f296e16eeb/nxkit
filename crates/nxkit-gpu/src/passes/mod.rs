//! Compute, copy and copy-back passes.
//!
//! Dispatches run the translated compute shaders with an argument buffer that carries the dispatch size and the
//! uniform buffer table in its driver block (see `bindless`). Copies are texture to texture on the GPU. A copy
//! between a scaled and an unscaled texture is not encoded. A CPU read of a texture the GPU wrote swizzles the
//! texture back into guest memory (R4).

mod copy;

use self::copy::copy;

use nxkit_metal::{CommandBuffer, ComputeCommand, ComputePass, Scratch};
use nxkit_nvn::frame::Cmd;
use nxkit_nvn::passes::{Pass, PassKind};
use nxkit_nvn::resource::{Residency, TexId};

use crate::bindless::{ScratchSink, StageInputs, fill_args};
use crate::detail::Stage;
use crate::draws::Shared;
use crate::encode::Tables;
use crate::stats::{EncodeStats, Skip};

fn dispatch(
    sh: &Shared<'_>,
    cp: &mut ComputePass<'_>,
    scratch: &Scratch,
    n: u32,
) -> Result<(), Skip> {
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
        blendz: 0,
    };
    let args = fill_args(
        &p,
        &inputs,
        &sh.env,
        &mut ScratchSink::new(scratch, sh.uploaded, None).with_preloaded(sh.preloaded),
    )?;
    let enc = |cp: &mut ComputePass<'_>, c| cp.encode(&c).map_err(|_| Skip::Encode);
    enc(cp, ComputeCommand::SetPipeline(pipeline))?;
    enc(
        cp,
        ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: args,
        },
    )?;
    enc(
        cp,
        ComputeCommand::DispatchThreadgroups {
            groups: dd.groups,
            threads: p.threadgroup,
        },
    )
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
    if let Some(dep) = sh.barriers.before.get(index).copied().flatten()
        && cp.encode(&ComputeCommand::Depend(dep)).is_err()
    {
        stats.skip(Skip::Encode);
    }
    let mut native_here = None;
    for i in pass.cmds.clone() {
        let Some(cmd) = sh.frame.cmds.get(i as usize).copied() else {
            continue;
        };
        let ord = sh.ordinals.get(i as usize).copied().unwrap_or_default();
        if sh.frame.skips(i) {
            stats.render_cond_other += 1;
            continue;
        }
        if let (PassKind::Compute, Cmd::Dispatch { .. }, Some(p)) = (pass.kind, cmd, sh.native)
            && p.is_anchor(ord.dispatch)
        {
            native_here = Some(p);
        }
        let r = match (pass.kind, cmd) {
            (PassKind::Compute, Cmd::Dispatch { .. }) => {
                dispatch(sh, &mut cp, scratch, ord.dispatch)
            }
            (PassKind::Copy, Cmd::Copy { .. } | Cmd::Downsample { .. }) => {
                copy(sh, &mut cp, scratch, cmd, ord.copy)
            }
            _ => Ok(()),
        };
        if crate::diag("comp")
            && crate::diag::sampled()
            && let (PassKind::Compute, Some(dd)) =
                (pass.kind, sh.detail.dispatches.get(ord.dispatch as usize))
        {
            eprintln!(
                "DIAG comp pass {index} cmd {i} program {:?} groups {:?} textures {} sbufs {} result {:?}",
                dd.program,
                dd.groups,
                dd.textures.len(),
                dd.sbufs.len(),
                r
            );
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
        let Some(Cmd::CpuRead(id)) = sh.frame.cmds.get(i as usize).copied() else {
            continue;
        };
        let (Ok(rec), Some(res)) = (sh.table.record(id), sh.textures.get(&id)) else {
            continue;
        };
        let written_here = sh.first_write.get(&id).is_some_and(|w| *w < index);
        if !(rec.residency == Residency::GpuNewer || written_here) || done.contains(&id) {
            continue;
        }
        match sh.transfer.copy_back(cb, scratch, sh.pools, rec, res) {
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
