//! Render pass encoding.
//!
//! A render pass of the plan becomes one Metal render pass. The attachments, load actions, store actions and the
//! memoryless choice come from the analysis. The draws go in one by one, or a replayed recording goes in as one
//! indirect command buffer execution. A [`Tracker`] remembers the encoder state, so that a draw only sets what
//! changed.
//!
//! The viewport and the scissor rectangle of NVN are in the size of the game. When the attachments are scaled (R19)
//! the pass multiplies them by the scale of the frame. The walker converts the viewport direction for the device
//! origin and the supported viewport swizzle. Scissor coordinates still name the same memory rows.

use std::sync::PoisonError;

use nxkit_metal::{
    ArgumentTable, ColorAttachment, CommandBuffer, Context, CullMode, Dependency, DepthAttachment,
    DepthBias, DepthClip, DepthStencilDesc, FillMode, LoadAction as MLoad, MetalError,
    RenderCommand, RenderPass, RenderPassDesc, Scissor, Scope, Scratch, Stages, StencilAttachment,
    Storage, StoreAction as MStore, Viewport, Visibility, Winding,
};
use nxkit_nvn::frame::Cmd;
use nxkit_nvn::passes::{LoadAction, Pass, Slot, StoreAction};
use nxkit_nvn::resource::TexId;

use crate::bindless::{ArgsMemo, ScratchSink};
use crate::detail::{ClearDetail, DrawArgs, DynamicDetail, RecordingRun};
use crate::draws::{Geom, PassCtx, Prep, Shared, prepare};
use crate::indirect::Prepass;
use crate::pipelines::Targets;
use crate::stats::{EncodeStats, Skip};
use crate::textures::BlockTarget;
use crate::translate;

mod blocks;
mod diag;
mod emit;
mod setup;
#[cfg(test)]
mod tests;
mod tracker;

use diag::*;
use emit::*;
use setup::*;
use tracker::*;

/// The argument tables of one encoding task. A vertex and a fragment stage need their own table. The mesh stage of a
/// mesh draw shares the vertex table and the object stage (a task shader) has the third.
pub(crate) struct Tables {
    pub(crate) vertex: ArgumentTable,
    pub(crate) fragment: ArgumentTable,
    pub(crate) object: ArgumentTable,
    /// The table of a copy pass: the kernel of a stretching texture copy binds two textures and a buffer.
    pub(crate) copy: ArgumentTable,
}

impl Tables {
    /// Makes the tables. A translated shader binds at most 31 buffers and no textures, since textures are bindless.
    pub(crate) fn new(ctx: &Context) -> Result<Self, MetalError> {
        Ok(Self {
            vertex: ArgumentTable::new(ctx, 31, 0)?,
            fragment: ArgumentTable::new(ctx, 31, 0)?,
            object: ArgumentTable::new(ctx, 31, 0)?,
            copy: ArgumentTable::new(ctx, 31, 2)?,
        })
    }
}

pub(super) fn draw_range(sh: &Shared<'_>, pass: &Pass) -> std::ops::Range<u32> {
    let first = sh
        .ordinals
        .get(pass.cmds.start as usize)
        .map_or(0, |o| o.draw);
    let n = pass
        .cmds
        .clone()
        .filter(|i| matches!(sh.frame.cmds.get(*i as usize), Some(Cmd::Draw { .. })))
        .count();
    first..first + n as u32
}

/// Which draws of `range` (the draws of `pass`) a false render condition leaves out, by their place in the range.
/// None when the frame has no such command.
fn disabled_draws(sh: &Shared<'_>, pass: &Pass, range: &std::ops::Range<u32>) -> Option<Vec<bool>> {
    if sh.frame.disabled.is_empty() {
        return None;
    }
    let off: Vec<bool> = pass
        .cmds
        .clone()
        .filter(|i| matches!(sh.frame.cmds.get(*i as usize), Some(Cmd::Draw { .. })))
        .map(|i| sh.frame.skips(i))
        .collect();
    (off.len() == range.len() && off.contains(&true)).then_some(off)
}

/// Sets the visibility offset that the next draws count into (None: they do not count). False when the encoder
/// refused it: the draws after it are not counted.
fn set_counting(
    rp: &mut RenderPass<'_>,
    counting: &mut Option<u64>,
    want: Option<u64>,
    stats: &mut EncodeStats,
) -> bool {
    if *counting == want {
        return true;
    }
    let cmd = RenderCommand::SetVisibility {
        counting: want.is_some(),
        offset: want.unwrap_or(0),
    };
    if rp.encode(&cmd).is_err() {
        stats.skipped[Skip::Encode as usize] += 1;
        return false;
    }
    *counting = want;
    true
}

/// Encodes one render pass into `cb`.
pub(crate) fn render_pass(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    tables: &Tables,
    scratch: &Scratch,
    index: usize,
    pass: &Pass,
    stats: &mut EncodeStats,
) {
    let range = draw_range(sh, pass);
    DIAG_PASS.with(|c| c.set(index));
    if crate::diag::tex_filtered() {
        let focus = sh.plan.attachments_of(pass).iter().any(|a| {
            sh.textures.get(&a.tex).is_some_and(|r| {
                crate::diag::tex_match(
                    r.desc.format,
                    r.desc.width,
                    r.desc.height,
                    r.desc.array_layers,
                )
            })
        });
        FOCUS_PASS.with(|c| c.set(focus));
    }
    let lost = |stats: &mut EncodeStats, why: Skip| {
        for n in range.clone() {
            stats.skip_draw(sh.detail.draws.get(n as usize).map(|d| &d.args), why);
        }
    };
    if crate::diag("barr") && crate::diag::sampled() {
        let known: std::collections::HashSet<TexId> = sh
            .plan
            .reads_of(pass)
            .iter()
            .copied()
            .chain(sh.plan.attachments_of(pass).iter().map(|a| a.tex))
            .collect();
        let mut missing: std::collections::BTreeSet<TexId> = std::collections::BTreeSet::new();
        let mut bound = 0usize;
        for n in range.clone() {
            if let Some(d) = sh.detail.draws.get(n as usize) {
                for t in d.textures.iter().filter(|t| t.handle != 0) {
                    bound += 1;
                    if let Some(id) = crate::barr::texture_of(t.handle)
                        && !known.contains(&id)
                    {
                        missing.insert(id);
                    }
                }
            }
        }
        eprintln!(
            "DIAG barr pass {index} cmds {:?} draws {} bound {bound} planner_reads {} missing {missing:?}",
            pass.cmds,
            range.len(),
            sh.plan.reads_of(pass).len()
        );
    }
    let s = setup(sh, pass);
    if sh
        .plan
        .attachments_of(pass)
        .iter()
        .any(|a| crate::watched(a.tex))
    {
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 40 {
            let atts: Vec<_> = sh
                .plan
                .attachments_of(pass)
                .iter()
                .map(|a| {
                    (
                        a.tex,
                        a.slot,
                        sh.textures
                            .get(&a.tex)
                            .map(|r| (r.desc.width, r.desc.height, r.desc.format)),
                    )
                })
                .collect();
            eprintln!(
                "DIAG watch pass {index} cmds {:?} draws {} setup {:?} atts {atts:?}",
                pass.cmds,
                range.len(),
                s.as_ref().err()
            );
        }
    }
    let s = match s {
        Ok(s) => s,
        Err(why) => return lost(stats, why),
    };
    // The block shadows that the pass loads get the blocks of their compressed texture first.
    let loads: Vec<&BlockTarget> = s.blocks.iter().filter(|b| b.1).map(|b| &b.0).collect();
    if !blocks::copy(sh.ctx, cb, &tables.vertex, &loads, blocks::Way::Load) {
        return lost(stats, Skip::Copy);
    }
    // The GPU driven draws of the pass get their copied arguments before the pass starts.
    let pre = crate::indirect::prepass(sh, cb, &tables.vertex, scratch, range.clone(), stats);
    // The draws of a slot of the GPU counters count their samples (`queries`).
    let vis = sh.queries.and_then(|q| Some((q, q.pass(index)?)));
    let visibility = vis.as_ref().map(|(_, v)| v.buffer);
    let scan = crate::nanscan::before(sh, cb, index, pass, range.clone(), &s.desc);
    let Ok(mut rp) =
        cb.begin_render_pass_counted(&s.desc, &tables.vertex, &tables.fragment, visibility)
    else {
        return lost(stats, Skip::Encode);
    };
    stats.render_passes += 1;
    if crate::diag::gpu_labels() {
        let first = s
            .desc
            .colors
            .iter()
            .flatten()
            .map(|c| c.texture.desc())
            .next()
            .or_else(|| s.desc.depth.as_ref().map(|d| d.texture.desc()));
        let what = first.map_or_else(String::new, |d| {
            format!("{}x{} {:?}", d.width, d.height, d.format)
        });
        rp.set_label(&format!("r{index} {what} d{}", range.len()));
    }
    if let Some(dep) = sh.barriers.before.get(index).copied().flatten()
        && rp.encode(&RenderCommand::Depend(dep)).is_err()
    {
        stats.skipped[Skip::Encode as usize] += 1;
    }
    if !loads.is_empty() {
        let dep = Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::BLIT,
            before: Stages::FRAGMENT,
            visibility: Visibility::Device,
        };
        if rp.encode(&RenderCommand::Depend(dep)).is_err() {
            stats.skipped[Skip::Encode as usize] += 1;
        }
    }
    // The barrier planner follows textures only, and it holds the vertex and fragment stages. An indirect draw reads
    // arguments that a dispatch or a copy wrote (culling), the pre-pass wrote the copies, and a task or mesh shader
    // reads storage buffers and textures in stages that the planner does not hold. Such a pass waits for them.
    if pre.gpu_driven {
        let dep = Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::DISPATCH | Stages::BLIT | Stages::FRAGMENT,
            before: Stages::VERTEX | Stages::OBJECT | Stages::MESH,
            visibility: Visibility::Device,
        };
        match rp.encode(&RenderCommand::Depend(dep)) {
            Ok(()) => stats.barriers_emitted += 1,
            Err(_) => stats.skipped[Skip::Encode as usize] += 1,
        }
    }
    let mut tr = Tracker::new(s.pc.size);
    let mut memo = ArgsMemo::default();
    let off = disabled_draws(sh, pass, &range);
    let is_off = |n: u32| off.as_ref().is_some_and(|o| o[(n - range.start) as usize]);
    let counts = |n: u32| {
        vis.as_ref().and_then(|(q, v)| match q.draw_slot(n) {
            Some(slot) => v.offset(slot),
            None => q.mesh_offset(n),
        })
    };
    // The visibility offset that counts now, None while nothing counts.
    let mut counting: Option<u64> = None;
    let mut n = range.start;
    while n < range.end {
        if is_off(n) {
            stats.render_cond_draws += 1;
            n += 1;
            continue;
        }
        if let Some(run) = sh
            .run_at
            .get(&n)
            .filter(|r| n + r.draws <= range.end && !crate::diag::skip_list_on())
            && (n..n + run.draws).all(|d| !is_off(d) && counts(d).is_none())
            && set_counting(&mut rp, &mut counting, None, stats)
            && run_icb(sh, &s.pc, &mut rp, &mut tr, run, stats)
        {
            n += run.draws;
            continue;
        }
        let want = counts(n);
        let counted = vis.is_some() && set_counting(&mut rp, &mut counting, want, stats);
        let before = stats.draws;
        draw(
            sh,
            &s.pc,
            &mut rp,
            &mut tr,
            scratch,
            &mut memo,
            &pre,
            &tables.object,
            n,
            stats,
        );
        if counted
            && want.is_some()
            && stats.draws > before
            && let Some(((q, _), slot)) = vis.as_ref().zip(sh.queries.and_then(|q| q.draw_slot(n)))
        {
            q.encoded(slot);
        }
        if counted
            && want.is_some()
            && stats.draws > before
            && let Some((q, _)) = vis.as_ref()
        {
            q.mesh_encoded(n);
        }
        n += 1;
    }
    if let Some(p) = sh.prof {
        p.end_pass_render(&mut rp, index);
    }
    rp.end();
    // The barrier plan counts the blit stage as a writer of a compressed attachment (`BarrierState::plan`).
    let stores: Vec<&BlockTarget> = s.blocks.iter().filter(|b| b.2).map(|b| &b.0).collect();
    if !blocks::copy(sh.ctx, cb, &tables.vertex, &stores, blocks::Way::Store) {
        stats.skipped[Skip::Copy as usize] += stores.len() as u32;
    }
    if let Some(scan) = scan {
        scan.after(sh, cb);
    }
    if crate::lum::active() && FOCUS_PASS.with(std::cell::Cell::get) {
        for a in sh.plan.attachments_of(pass) {
            if matches!(a.slot, Slot::Color(_))
                && a.store == StoreAction::Store
                && let Slot::Color(slot) = a.slot
            {
                let Some(Some(color)) = s.desc.colors.get(usize::from(slot)) else {
                    continue;
                };
                crate::lum::capture(
                    sh.ctx,
                    cb,
                    &tables.vertex,
                    pass.cmds.start,
                    a.tex,
                    &color.texture,
                    color.depth_plane,
                );
            }
        }
    }
}
