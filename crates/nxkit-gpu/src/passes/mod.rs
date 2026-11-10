//! Compute, copy and copy-back passes.
//!
//! Dispatches run the translated compute shaders with an argument buffer that carries the dispatch size and the
//! uniform buffer table in its driver block (see `bindless`). Copies are texture to texture on the GPU, and a copy
//! with a destination box of another size stretches the source box (`scale`). A copy between a scaled and an
//! unscaled texture is not encoded. A CPU read of a texture the GPU wrote swizzles the
//! texture back into guest memory (R4).

mod buffers;
mod copy;
mod indirect_dispatch;
mod scale;

use self::copy::copy;

use nxkit_metal::{
    CommandBuffer, ComputeCommand, ComputePass, Dependency, Scope, Scratch, Stages, Visibility,
};
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
    if let Some(indirect) = dd.indirect {
        return indirect_dispatch::encode(sh, cp, scratch, &p, pipeline, args, indirect);
    }
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
    let nan = if pass.kind == PassKind::Compute {
        crate::nanscan::compute_before(sh, cb, index, pass)
    } else {
        None
    };
    let table = if pass.kind == PassKind::Copy {
        &tables.copy
    } else {
        &tables.vertex
    };
    let Ok(mut cp) = cb.begin_compute_pass(table) else {
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
    // The kernel of a buffer copy or fill reads what earlier dispatches, copies and draws wrote and writes what later
    // work reads. The barrier planner follows textures only, so the pass waits for all earlier work.
    if pass.kind == PassKind::Copy
        && pass
            .cmds
            .clone()
            .any(|i| matches!(sh.frame.cmds.get(i as usize), Some(Cmd::BufferCopy)))
    {
        let dep = Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::ALL,
            before: Stages::DISPATCH | Stages::BLIT,
            visibility: Visibility::Device,
        };
        if cp.encode(&ComputeCommand::Depend(dep)).is_err() {
            stats.skip(Skip::Encode);
        }
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
            (PassKind::Copy, Cmd::Copy { .. } | Cmd::BufferCopy | Cmd::Downsample { .. }) => {
                copy(sh, &mut cp, scratch, cmd, ord.copy)
            }
            _ => Ok(()),
        };
        if crate::diag("comp")
            && crate::diag::sampled()
            && let (PassKind::Compute, Some(dd)) =
                (pass.kind, sh.detail.dispatches.get(ord.dispatch as usize))
        {
            let images: Vec<String> = dd
                .images
                .iter()
                .filter(|t| t.handle != 0)
                .map(|t| {
                    let id = crate::barr::texture_of(t.handle);
                    let shape = id.and_then(|id| sh.textures.get(&id)).map(|r| {
                        let d = r.tex.desc();
                        format!("{}x{}x{} {:?}", d.width, d.height, d.depth, d.format)
                    });
                    format!(
                        "{}:{id:?}:{}",
                        t.slot,
                        shape.as_deref().unwrap_or("unknown")
                    )
                })
                .collect();
            eprintln!(
                "DIAG comp pass {index} cmd {i} program {:?} groups {:?}{} textures {} sbufs {} images [{}] result {:?}",
                dd.program,
                dd.groups,
                dd.indirect
                    .map_or_else(String::new, |a| format!(" indirect {a:#x}")),
                dd.textures.len(),
                dd.sbufs.len(),
                images.join(", "),
                r
            );
        }
        if crate::diag("ubodump")
            && crate::diag::sampled()
            && let (PassKind::Compute, Some(dd)) =
                (pass.kind, sh.detail.dispatches.get(ord.dispatch as usize))
            && (crate::diag::fs_programs().contains(&dd.program) || crate::knob("ubodumpall"))
        {
            ubo_dump(sh, index, i, dd);
        }
        if let Err(why) = r {
            stats.skip(why);
        }
    }
    cp.end();
    if let Some(n) = nan {
        n.after(sh, cb);
    }
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

/// `DIAG ubodump`: the 64-bit values at byte offsets 192 to 448 (step 16) of each uniform buffer of a dispatch, and
/// its push constants. At most 64 lines per frame.
fn ubo_dump(sh: &Shared<'_>, pass: usize, cmd: u32, dd: &crate::detail::DispatchDetail) {
    use std::sync::atomic::AtomicU64;
    static BUDGET: AtomicU64 = AtomicU64::new(0);
    let max = if crate::knob("ubodumpall") { 400 } else { 64 };
    for s in &dd.sbufs {
        if !crate::diag::sample_budget(&BUDGET, max) {
            return;
        }
        eprintln!(
            "DIAG ubodump pass {pass} cmd {cmd} program {} sbuf {} addr {:#x} size {}",
            dd.program, s.slot, s.address, s.size
        );
    }
    for u in &dd.ubos {
        if !crate::diag::sample_budget(&BUDGET, max) {
            return;
        }
        let want = 456usize.min(u.size as usize);
        let bytes: Option<Vec<u8>> = match &u.snapshot {
            Some(s) => Some(s.iter().copied().take(want).collect()),
            None => sh.guest_offset(u.address, want as u64).and_then(|off| {
                let mut b = vec![0u8; want];
                sh.guest.read(off, &mut b).ok().map(|()| b)
            }),
        };
        let mut vals = String::new();
        if let Some(b) = &bytes {
            for off in (192..=448usize).step_by(16) {
                if let Some(w) = b.get(off..off + 8) {
                    let v = u64::from_le_bytes(w.try_into().unwrap_or([0; 8]));
                    vals.push_str(&format!(
                        "{}@{off} {v:#x}",
                        if vals.is_empty() { "" } else { " " }
                    ));
                }
            }
        }
        eprintln!(
            "DIAG ubodump pass {pass} cmd {cmd} program {} cbuf {} addr {:#x} size {} u64 [{}]{}",
            dd.program,
            u.slot,
            u.address,
            u.size,
            vals,
            if bytes.is_none() { " unreadable" } else { "" }
        );
    }
    if let Some(p) = &dd.push {
        let words: Vec<String> = p
            .as_chunks::<4>()
            .0
            .iter()
            .take(16)
            .map(|c| format!("{:#010x}", u32::from_le_bytes(*c)))
            .collect();
        eprintln!(
            "DIAG ubodump pass {pass} cmd {cmd} program {} push {} bytes u32 [{}]",
            dd.program,
            p.len(),
            words.join(" ")
        );
    }
}
