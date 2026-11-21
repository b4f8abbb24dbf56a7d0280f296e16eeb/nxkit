//! The replay of a submit into one Metal 4 command buffer (the reference layer's `rs_t` and `replay_ops`).
//!
//! Each primary command buffer starts with fresh state, as Vulkan's command buffers do; its secondaries replay in
//! place. The commands run in order through three kinds of segments:
//!
//! - a render segment ([`draw`]) from `vkCmdBeginRenderPass`, or from the draw that resumes a pass after a cut, to the
//!   end of the pass or the next cut ([`crate::passplan`]);
//! - a compute segment ([`transfer`]) for a run of copies, fills, updates and dispatches outside a render pass;
//! - one render pass per subresource for `vkCmdClearColorImage` and `vkCmdClearDepthStencilImage`.
//!
//! Metal 4 tracks no hazards, so each Metal pass starts with a barrier after the earlier work of the queue, and each
//! command of a compute segment after the one before it. Vulkan's barriers are then already kept.

mod attach;
mod draw;
mod images;
mod transfer;

use std::collections::HashMap;
use std::sync::Arc;

use nxkit_metal::{ArgumentTable, CommandBuffer, Context, Scratch, Texture};
use nxkit_vk::backend::SetContents;
use nxkit_vk::cmd::{BindPoint, ClearAttachment, Cmd, IndexType, RenderPassBegin, StencilValue};
use nxkit_vk::desc::{BufferRef, PipelineId, Rect2D, Viewport};

use crate::bind::{self, BoundSet, SETS};
use crate::log::OnceLog;
use crate::make::stage;
use crate::objects::Objects;

/// The argument tables of the passes: one per render stage and one for compute. Metal 4 copies a table's bindings at
/// each draw and dispatch, so the same tables serve every pass.
pub(crate) struct Tables {
    /// The vertex stage.
    pub(crate) vertex: ArgumentTable,
    /// The fragment stage.
    pub(crate) fragment: ArgumentTable,
    /// Compute passes.
    pub(crate) compute: ArgumentTable,
}

impl Tables {
    /// Makes the tables with every slot of the shader table's format.
    pub(crate) fn new(ctx: &Context) -> nxkit_metal::Result<Self> {
        use nxkit_abi::vk_table::{BUFFER_SLOTS, SAMPLER_SLOTS, TEXTURE_SLOTS};
        let t = || ArgumentTable::with_samplers(ctx, BUFFER_SLOTS, TEXTURE_SLOTS, SAMPLER_SLOTS);
        Ok(Self {
            vertex: t()?,
            fragment: t()?,
            compute: t()?,
        })
    }
}

/// The counters of a replay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stats {
    /// Draws encoded.
    pub(crate) draws: u64,
    /// Draws skipped (no pipeline, no pass, a binding that failed).
    pub(crate) skipped: u64,
    /// Metal render passes.
    pub(crate) passes: u64,
    /// Copies, fills and updates.
    pub(crate) copies: u64,
    /// Dispatches.
    pub(crate) dispatches: u64,
}

/// What a replay reads and writes besides the commands.
pub(crate) struct Replay<'a> {
    /// The device.
    pub(crate) ctx: &'a Context,
    /// The Metal objects.
    pub(crate) objects: &'a Objects,
    /// The descriptor sets of the submission.
    pub(crate) sets: &'a HashMap<u64, SetContents>,
    /// The argument tables.
    pub(crate) tables: &'a Tables,
    /// The scratch buffer of the frame: push constants, updates and fills.
    pub(crate) scratch: Arc<Scratch>,
    /// The messages.
    pub(crate) log: &'a OnceLog,
    /// The counters.
    pub(crate) stats: Stats,
}

/// The render pass that the commands are in.
pub(crate) struct Pass {
    /// The begin of the pass.
    pub(crate) begin: Arc<RenderPassBegin>,
    /// The texture of each colour attachment of the subpass.
    pub(crate) colors: Vec<Option<Texture>>,
    /// The texture of the depth-stencil attachment.
    pub(crate) depth: Option<Texture>,
    /// The single-sample texture that each multisample colour attachment resolves into.
    pub(crate) resolves: Vec<Option<Texture>>,
    /// The render target size: the framebuffer's, at most each attachment's.
    pub(crate) size: (u32, u32),
    /// A cut ended the Metal pass; the next draw resumes it.
    pub(crate) cut: bool,
    /// An attachment is missing: the pass draws nothing.
    pub(crate) broken: bool,
    /// The whole-attachment clears that the next segment makes with its load actions.
    pub(crate) clears: Vec<ClearAttachment>,
}

impl Pass {
    /// Resolves the attachments of a begin.
    fn new(begin: &Arc<RenderPassBegin>, objects: &Objects, log: &OnceLog) -> Self {
        let fb = &begin.framebuffer;
        let mut broken = false;
        let mut size = (fb.width.max(1), fb.height.max(1));
        let mut tex = |a: Option<u32>| {
            let a = a?;
            let t = fb
                .attachments
                .get(a as usize)
                .and_then(|at| objects.texture(at.view));
            match t {
                Some(t) => {
                    let d = t.desc();
                    size = (size.0.min(d.width), size.1.min(d.height));
                    Some(t.clone())
                }
                None => {
                    broken = true;
                    None
                }
            }
        };
        let colors = begin.pass.colors.iter().map(|c| tex(*c)).collect();
        let depth = tex(begin.pass.depth);
        let resolves = begin
            .pass
            .resolves
            .iter()
            .map(|r| {
                let at = fb.attachments.get((*r)? as usize)?;
                objects.texture(at.view).cloned()
            })
            .collect();
        if broken {
            log.once(
                "pass attachment",
                "[RED] a render pass names an attachment that has no texture: its draws are skipped",
            );
        }
        Self {
            begin: begin.clone(),
            colors,
            depth,
            resolves,
            size,
            cut: false,
            broken,
            clears: Vec::new(),
        }
    }
}

/// The state of one primary command buffer.
pub(crate) struct State {
    /// The graphics pipeline bound.
    pub(crate) graphics: Option<PipelineId>,
    /// The compute pipeline bound.
    pub(crate) compute: Option<PipelineId>,
    /// The descriptor sets of the graphics and the compute bind point.
    pub(crate) sets: [[Option<BoundSet>; SETS]; 2],
    /// The vertex buffers by binding.
    pub(crate) vertex_buffers: [Option<BufferRef>; 16],
    /// The index buffer.
    pub(crate) index: Option<(BufferRef, IndexType)>,
    /// The dynamic viewport. It persists across render passes, as in Vulkan; the render target stands in until the game
    /// sets one (Super Mario Galaxy sets a 1280x720 viewport once and draws through several passes).
    pub(crate) viewport: Option<Viewport>,
    /// The dynamic scissor.
    pub(crate) scissor: Option<Rect2D>,
    /// The push constants of the vertex, fragment and compute stage.
    pub(crate) push: [[u8; 256]; 3],
    /// The dynamic blend constants.
    pub(crate) blend: Option<[f32; 4]>,
    /// The dynamic depth bias (constant, clamp, slope).
    pub(crate) bias: Option<[f32; 3]>,
    /// The dynamic stencil compare mask, write mask and reference, front and back.
    pub(crate) stencil: [[Option<u32>; 2]; 3],
    /// The render pass that the commands are in.
    pub(crate) pass: Option<Pass>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            graphics: None,
            compute: None,
            sets: Default::default(),
            vertex_buffers: [None; 16],
            index: None,
            viewport: None,
            scissor: None,
            push: [[0; 256]; 3],
            blend: None,
            bias: None,
            stencil: [[None; 2]; 3],
            pass: None,
        }
    }
}

impl State {
    /// Applies a command that only changes state. Gives false for a command that does work.
    pub(crate) fn apply(&mut self, c: &Cmd, sets: &HashMap<u64, SetContents>) -> bool {
        match c {
            Cmd::BindPipeline { bind, pipeline } => match bind {
                BindPoint::Graphics => self.graphics = Some(*pipeline),
                BindPoint::Compute => self.compute = Some(*pipeline),
            },
            Cmd::BindDescriptorSets {
                bind,
                first,
                sets: handles,
                dynamic_offsets,
            } => {
                let point = usize::from(*bind == BindPoint::Compute);
                bind::bind_sets(
                    &mut self.sets[point],
                    *first,
                    handles,
                    dynamic_offsets,
                    |h| sets.get(&h).map(|s| s.layout.dynamic_count),
                );
            }
            Cmd::BindVertexBuffers { first, buffers } => {
                for (i, b) in buffers.iter().enumerate() {
                    if let Some(slot) = self.vertex_buffers.get_mut(*first as usize + i) {
                        *slot = Some(*b);
                    }
                }
            }
            Cmd::BindIndexBuffer { buffer, index_type } => {
                self.index = Some((*buffer, *index_type))
            }
            Cmd::SetViewport { first, viewports } => {
                if *first == 0
                    && let Some(v) = viewports.first()
                {
                    self.viewport = Some(*v);
                }
            }
            Cmd::SetScissor { first, rects } => {
                if *first == 0
                    && let Some(r) = rects.first()
                {
                    self.scissor = Some(*r);
                }
            }
            Cmd::SetBlendConstants(c) => self.blend = Some(*c),
            Cmd::SetDepthBias(b) => self.bias = Some(*b),
            Cmd::SetStencil { what, faces, value } => {
                let i = match what {
                    StencilValue::CompareMask => 0,
                    StencilValue::WriteMask => 1,
                    StencilValue::Reference => 2,
                };
                for (face, bit) in [(0, 1), (1, 2)] {
                    if faces & bit != 0 {
                        self.stencil[i][face] = Some(*value);
                    }
                }
            }
            Cmd::PushConstants {
                stages,
                offset,
                data,
            } => {
                for (i, bit) in [stage::VERTEX, stage::FRAGMENT, stage::COMPUTE]
                    .into_iter()
                    .enumerate()
                {
                    if stages & bit == 0 {
                        continue;
                    }
                    let at = (*offset as usize).min(256);
                    let n = data.len().min(256 - at);
                    if let (Some(dst), Some(src)) =
                        (self.push[i].get_mut(at..at + n), data.get(..n))
                    {
                        dst.copy_from_slice(src);
                    }
                }
            }
            // Metal draws lines one pixel wide; the depth bounds test is reported where a pipeline turns it on. Events
            // and queries are the front end's and the completion's (`crate::queries`).
            Cmd::SetLineWidth(_)
            | Cmd::SetDepthBounds(_)
            | Cmd::SetEvent(_)
            | Cmd::ResetEvent(_)
            | Cmd::ResetQueryPool { .. }
            | Cmd::BeginQuery { .. }
            | Cmd::EndQuery { .. }
            | Cmd::WriteTimestamp { .. }
            | Cmd::CopyQueryPoolResults { .. } => {}
            _ => return false,
        }
        true
    }
}

/// Replays the commands of one primary command buffer (secondaries in place, [`crate::passplan::flatten`]).
pub(crate) fn primary(r: &mut Replay<'_>, cb: &mut CommandBuffer, cmds: &[&Cmd]) {
    let mut st = State::default();
    let mut i = 0;
    while let Some(&c) = cmds.get(i) {
        // A whole-attachment clear inside the pass: the next segment clears with its load actions, at once.
        if let Some(p) = &mut st.pass
            && p.cut
            && !p.broken
            && !p.clears.is_empty()
        {
            let clears = std::mem::take(&mut p.clears);
            i = draw::segment(r, &mut st, cb, cmds, i, Some(&clears));
            continue;
        }
        match c {
            Cmd::BeginRenderPass(b) => {
                st.pass = Some(Pass::new(b, r.objects, r.log));
                i = draw::segment(r, &mut st, cb, cmds, i + 1, None);
            }
            Cmd::EndRenderPass => {
                st.pass = None;
                i += 1;
            }
            c if c.is_draw() => match &st.pass {
                Some(p) if !p.broken => i = draw::segment(r, &mut st, cb, cmds, i, Some(&[])),
                Some(_) => {
                    r.stats.skipped += 1;
                    i += 1;
                }
                None => {
                    r.log.once(
                        "draw outside",
                        "[RED] a draw outside a render pass is skipped",
                    );
                    r.stats.skipped += 1;
                    i += 1;
                }
            },
            Cmd::ClearColorImage { .. } | Cmd::ClearDepthStencilImage { .. } => {
                images::clear_image(r, cb, c);
                i += 1;
            }
            Cmd::ResolveImage { .. } => {
                images::resolve_image(r, cb, c);
                i += 1;
            }
            Cmd::ClearAttachments { .. } | Cmd::PipelineBarrier => i += 1,
            c if transfer::is_compute(c) => i = transfer::segment(r, &mut st, cb, cmds, i),
            c => {
                if !st.apply(c, r.sets) {
                    r.log.once(
                        format!("cmd {}", c.name()),
                        format_args!("[RED] {} is not implemented", c.name()),
                    );
                }
                i += 1;
            }
        }
    }
}
