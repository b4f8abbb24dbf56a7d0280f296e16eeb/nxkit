//! Command pools and buffers, and the recording of render passes, bindings, dynamic state and draws.

use std::collections::HashSet;
use std::sync::Arc;

use crate::cmd::{BindPoint, Cmd, IndexType, RenderPassBegin, StencilValue};
use crate::consts::{bind_point, special};
use crate::desc::{PipelineId, Rect2D, Viewport};
use crate::handle::Kind;
use crate::info::RenderPassBeginInfo;
use crate::result::{Log, VkError, VkResult};

use super::Vk;
use super::state::{CbState, CommandBuffer, State};

/// The most viewports and scissors (Apple GPU family 5 and newer).
const MAX_VIEWPORTS: u32 = 16;

/// The most vertex buffer bindings.
const MAX_VERTEX_BINDINGS: u32 = 14;

/// A bind point of the game, or None (reported once) for one the layer does not have.
pub(crate) fn bind_of(log: &Log, bp: u32) -> Option<BindPoint> {
    match bp {
        bind_point::GRAPHICS => Some(BindPoint::Graphics),
        bind_point::COMPUTE => Some(BindPoint::Compute),
        other => {
            log.once(
                "bind point",
                format_args!("[RED] pipeline bind point {other} not implemented"),
            );
            None
        }
    }
}

impl Vk {
    /// Records the command that `f` makes from the state, unless it makes none.
    pub(crate) fn record(&self, cb: u64, f: impl FnOnce(&State) -> Option<Cmd>) {
        let mut s = self.lock();
        let Some(c) = f(&s) else {
            return;
        };
        let Some(b) = s.command_buffers.get_mut(cb) else {
            self.log.once(
                "record unknown",
                "[RED] a command is recorded into an unknown command buffer",
            );
            return;
        };
        if b.state != CbState::Recording {
            self.log.once(
                "record state",
                format_args!("[RED] {} recorded outside vkBeginCommandBuffer", c.name()),
            );
        }
        match c {
            Cmd::BeginRenderPass(_) => b.in_pass = true,
            Cmd::EndRenderPass => b.in_pass = false,
            _ => {}
        }
        b.recording.push(c);
    }

    /// `vkCreateCommandPool`.
    pub fn create_command_pool(&self) -> u64 {
        self.lock().command_pools.insert(HashSet::new())
    }

    /// `vkDestroyCommandPool`: its command buffers go with it, and their slots.
    pub fn destroy_command_pool(&self, pool: u64) {
        let freed: Vec<u64> = {
            let mut s = self.lock();
            let Some(cbs) = s.command_pools.remove(pool) else {
                return;
            };
            cbs.into_iter()
                .filter(|&c| s.command_buffers.remove(c).is_some())
                .collect()
        };
        for c in freed {
            self.slots.free(c);
        }
    }

    /// `vkResetCommandPool`: every command buffer of the pool goes back to the initial state.
    pub fn reset_command_pool(&self, pool: u64) -> VkResult<()> {
        let mut s = self.lock();
        let cbs: Vec<u64> = s
            .command_pools
            .get(pool)
            .ok_or_else(|| self.bad("command pool", pool))?
            .iter()
            .copied()
            .collect();
        for c in cbs {
            if let Some(b) = s.command_buffers.get_mut(c) {
                reset(b);
            }
        }
        Ok(())
    }

    /// `vkAllocateCommandBuffers`: each is a dispatchable slot. Secondary command buffers are recorded alike and replayed
    /// where `vkCmdExecuteCommands` names them.
    pub fn allocate_command_buffers(
        &self,
        pool: u64,
        level: u32,
        count: u32,
    ) -> VkResult<Vec<u64>> {
        if self.lock().command_pools.get(pool).is_none() {
            return Err(self.bad("command pool", pool));
        }
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            match self.slots.alloc(Kind::CommandBuffer) {
                Some(a) => out.push(a),
                None => {
                    for a in out {
                        self.slots.free(a);
                    }
                    return Err(VkError::OutOfHostMemory);
                }
            }
        }
        let mut s = self.lock();
        for &a in &out {
            s.command_buffers.insert_at(
                a,
                CommandBuffer {
                    pool,
                    secondary: level == special::LEVEL_SECONDARY,
                    state: CbState::Initial,
                    recording: Vec::new(),
                    frozen: Arc::from(Vec::new()),
                    in_pass: false,
                },
            );
        }
        if let Some(p) = s.command_pools.get_mut(pool) {
            p.extend(out.iter().copied());
        }
        Ok(out)
    }

    /// `vkFreeCommandBuffers`.
    pub fn free_command_buffers(&self, cbs: &[u64]) {
        let freed: Vec<u64> = {
            let mut s = self.lock();
            cbs.iter()
                .copied()
                .filter(|&c| match s.command_buffers.remove(c) {
                    Some(b) => {
                        if let Some(p) = s.command_pools.get_mut(b.pool) {
                            p.remove(&c);
                        }
                        true
                    }
                    None => false,
                })
                .collect()
        };
        for c in freed {
            self.slots.free(c);
        }
    }

    /// `vkBeginCommandBuffer`: starts a new recording (an implicit reset).
    pub fn begin_command_buffer(&self, cb: u64) -> VkResult<()> {
        let mut s = self.lock();
        let b = s
            .command_buffers
            .get_mut(cb)
            .ok_or_else(|| self.bad("command buffer", cb))?;
        reset(b);
        b.state = CbState::Recording;
        Ok(())
    }

    /// `vkEndCommandBuffer`: the recording becomes what a submit replays.
    pub fn end_command_buffer(&self, cb: u64) -> VkResult<()> {
        let mut s = self.lock();
        let b = s
            .command_buffers
            .get_mut(cb)
            .ok_or_else(|| self.bad("command buffer", cb))?;
        if b.in_pass && !b.secondary {
            self.log.once(
                "end in pass",
                "[RED] a command buffer ended inside a render pass",
            );
        }
        b.frozen = Arc::from(std::mem::take(&mut b.recording));
        b.state = CbState::Executable;
        Ok(())
    }

    /// `vkResetCommandBuffer`.
    pub fn reset_command_buffer(&self, cb: u64) -> VkResult<()> {
        let mut s = self.lock();
        reset(
            s.command_buffers
                .get_mut(cb)
                .ok_or_else(|| self.bad("command buffer", cb))?,
        );
        Ok(())
    }

    /// The commands that a submit of a command buffer replays now, for tests and diagnostics.
    pub fn recorded(&self, cb: u64) -> Option<Arc<[Cmd]>> {
        self.lock()
            .command_buffers
            .get(cb)
            .map(|b| b.frozen.clone())
    }

    /// `vkCmdBeginRenderPass`: the render pass and the framebuffer are resolved now.
    pub fn cmd_begin_render_pass(&self, cb: u64, info: &RenderPassBeginInfo) {
        self.record(cb, |s| {
            let (Some(pass), Some(fb)) = (
                s.render_passes.get(info.render_pass),
                s.framebuffers.get(info.framebuffer),
            ) else {
                self.log.once(
                    "begin pass",
                    "[RED] vkCmdBeginRenderPass names an unknown render pass or framebuffer",
                );
                return None;
            };
            Some(Cmd::BeginRenderPass(Arc::new(RenderPassBegin {
                pass: pass.clone(),
                framebuffer: fb.clone(),
                area: info.area,
                clear: info.clear.clone(),
            })))
        });
    }

    /// `vkCmdNextSubpass`: render passes have one subpass, so there is none to go to.
    pub fn cmd_next_subpass(&self, _cb: u64) {
        self.log.once(
            "next subpass",
            "[RED] vkCmdNextSubpass: render passes with several subpasses are not implemented",
        );
    }

    /// `vkCmdEndRenderPass`.
    pub fn cmd_end_render_pass(&self, cb: u64) {
        self.record(cb, |_| Some(Cmd::EndRenderPass));
    }

    /// `vkCmdBindPipeline`.
    pub fn cmd_bind_pipeline(&self, cb: u64, bp: u32, pipeline: u64) {
        self.record(cb, |s| {
            let bind = bind_of(&self.log, bp)?;
            if s.pipelines.get(pipeline).is_some_and(|p| p.bind != bind) {
                self.log.once(
                    "bind kind",
                    "[RED] a pipeline is bound at the other bind point",
                );
            }
            Some(Cmd::BindPipeline {
                bind,
                pipeline: PipelineId(pipeline),
            })
        });
    }

    /// `vkCmdBindDescriptorSets`: the sets by handle (their contents are read at submit) and the dynamic offsets.
    pub fn cmd_bind_descriptor_sets(
        &self,
        cb: u64,
        bp: u32,
        first: u32,
        sets: &[u64],
        dynamic_offsets: &[u32],
    ) {
        self.record(cb, |_| {
            let bind = bind_of(&self.log, bp)?;
            if first as usize + sets.len() > 8 {
                self.log.once(
                    "bind sets",
                    format_args!("[RED] descriptor sets {first}+{} bound, over 8", sets.len()),
                );
            }
            Some(Cmd::BindDescriptorSets {
                bind,
                first,
                sets: sets
                    .iter()
                    .copied()
                    .take(8usize.saturating_sub(first as usize))
                    .collect(),
                dynamic_offsets: dynamic_offsets.to_vec(),
            })
        });
    }

    /// `vkCmdBindVertexBuffers`: at most 14 bindings.
    pub fn cmd_bind_vertex_buffers(&self, cb: u64, first: u32, buffers: &[u64], offsets: &[u64]) {
        self.record(cb, |s| {
            let room = MAX_VERTEX_BINDINGS.saturating_sub(first) as usize;
            let buffers = buffers
                .iter()
                .zip(offsets)
                .take(room)
                .map(|(&b, &o)| s.buffer_ref(&self.log, b, o))
                .collect();
            Some(Cmd::BindVertexBuffers { first, buffers })
        });
    }

    /// `vkCmdBindIndexBuffer`.
    pub fn cmd_bind_index_buffer(&self, cb: u64, buffer: u64, offset: u64, index_type: u32) {
        self.record(cb, |s| {
            let index_type = match index_type {
                0 => IndexType::U16,
                1 => IndexType::U32,
                1_000_265_000 => IndexType::U8,
                other => {
                    self.log.once(
                        "index type",
                        format_args!("[RED] index type {other} not implemented"),
                    );
                    return None;
                }
            };
            Some(Cmd::BindIndexBuffer {
                buffer: s.buffer_ref(&self.log, buffer, offset),
                index_type,
            })
        });
    }

    /// `vkCmdSetViewport`: at most 16, and none past the 16th.
    pub fn cmd_set_viewport(&self, cb: u64, first: u32, viewports: Vec<Viewport>) {
        let n = viewports.len() as u32;
        if n == 0 || first >= MAX_VIEWPORTS || n > MAX_VIEWPORTS - first {
            return;
        }
        self.record(cb, |_| Some(Cmd::SetViewport { first, viewports }));
    }

    /// `vkCmdSetScissor`: as for viewports.
    pub fn cmd_set_scissor(&self, cb: u64, first: u32, rects: Vec<Rect2D>) {
        let n = rects.len() as u32;
        if n == 0 || first >= MAX_VIEWPORTS || n > MAX_VIEWPORTS - first {
            return;
        }
        self.record(cb, |_| Some(Cmd::SetScissor { first, rects }));
    }

    /// `vkCmdSetLineWidth`.
    pub fn cmd_set_line_width(&self, cb: u64, width: f32) {
        self.record(cb, |_| Some(Cmd::SetLineWidth(width)));
    }

    /// `vkCmdSetDepthBias`.
    pub fn cmd_set_depth_bias(&self, cb: u64, constant: f32, clamp: f32, slope: f32) {
        self.record(cb, |_| Some(Cmd::SetDepthBias([constant, clamp, slope])));
    }

    /// `vkCmdSetBlendConstants`.
    pub fn cmd_set_blend_constants(&self, cb: u64, c: [f32; 4]) {
        self.record(cb, |_| Some(Cmd::SetBlendConstants(c)));
    }

    /// `vkCmdSetDepthBounds`: recorded, but the test is not implemented (reported once).
    pub fn cmd_set_depth_bounds(&self, cb: u64, min: f32, max: f32) {
        self.log.once(
            "depth bounds",
            format_args!("[RED] depth bounds ({min}, {max}) not implemented"),
        );
        self.record(cb, |_| Some(Cmd::SetDepthBounds([min, max])));
    }

    /// `vkCmdSetStencilCompareMask`, `vkCmdSetStencilWriteMask`, `vkCmdSetStencilReference`.
    pub fn cmd_set_stencil(&self, cb: u64, what: StencilValue, faces: u32, value: u32) {
        self.record(cb, |_| Some(Cmd::SetStencil { what, faces, value }));
    }

    /// `vkCmdPushConstants`: the bytes are copied; at most 256 from offset 0.
    pub fn cmd_push_constants(&self, cb: u64, stages: u32, offset: u32, data: Vec<u8>) {
        if offset >= 256 || data.len() > (256 - offset) as usize {
            self.log.once(
                "push range",
                format_args!(
                    "[RED] push constant out of range: offset {offset} length {}",
                    data.len()
                ),
            );
            return;
        }
        self.record(cb, |_| {
            Some(Cmd::PushConstants {
                stages,
                offset,
                data,
            })
        });
    }

    /// `vkCmdDraw`.
    pub fn cmd_draw(
        &self,
        cb: u64,
        vertices: u32,
        instances: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        self.record(cb, |_| {
            Some(Cmd::Draw {
                vertices,
                instances,
                first_vertex,
                first_instance,
            })
        });
    }

    /// `vkCmdDrawIndexed`.
    pub fn cmd_draw_indexed(
        &self,
        cb: u64,
        indices: u32,
        instances: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        self.record(cb, |_| {
            Some(Cmd::DrawIndexed {
                indices,
                instances,
                first_index,
                vertex_offset,
                first_instance,
            })
        });
    }

    /// `vkCmdDrawIndirect` and `vkCmdDrawIndexedIndirect`: the argument layouts are Metal's.
    pub fn cmd_draw_indirect(
        &self,
        cb: u64,
        buffer: u64,
        offset: u64,
        count: u32,
        stride: u32,
        indexed: bool,
    ) {
        self.record(cb, |s| {
            Some(Cmd::DrawIndirect {
                buffer: s.buffer_ref(&self.log, buffer, offset),
                count,
                stride,
                indexed,
            })
        });
    }

    /// `vkCmdDrawIndirectCountAMD` and `vkCmdDrawIndexedIndirectCountAMD`: `VK_AMD_draw_indirect_count` is not
    /// advertised, so nothing is recorded (reported once).
    pub fn cmd_draw_indirect_count(&self, _cb: u64) {
        self.log.once("indirect count", "[RED] vkCmdDraw*IndirectCountAMD not supported (VK_AMD_draw_indirect_count is not advertised)");
    }

    /// `vkCmdDispatch`.
    pub fn cmd_dispatch(&self, cb: u64, x: u32, y: u32, z: u32) {
        self.record(cb, |_| Some(Cmd::Dispatch([x, y, z])));
    }

    /// `vkCmdDispatchBase`: Metal has no base workgroup, so a non-zero base is reported and skipped rather than run
    /// with wrong global ids.
    pub fn cmd_dispatch_base(&self, cb: u64, base: [u32; 3], groups: [u32; 3]) {
        if base != [0; 3] {
            self.log.once(
                "dispatch base",
                format_args!("[RED] dispatch with a base workgroup {base:?} not implemented"),
            );
            return;
        }
        self.cmd_dispatch(cb, groups[0], groups[1], groups[2]);
    }

    /// `vkCmdDispatchIndirect`.
    pub fn cmd_dispatch_indirect(&self, cb: u64, buffer: u64, offset: u64) {
        self.record(cb, |s| {
            Some(Cmd::DispatchIndirect(
                s.buffer_ref(&self.log, buffer, offset),
            ))
        });
    }
}

fn reset(b: &mut CommandBuffer) {
    b.recording.clear();
    b.frozen = Arc::from(Vec::new());
    b.state = CbState::Initial;
    b.in_pass = false;
}
