//! Command pools and command buffers, and the recorded commands of render passes, binds, dynamic state, draws and
//! dispatches. A recording call of a command buffer that the front end does not know records nothing (the front end
//! reports it); only the calls that return a `VkResult` fail on it.

use nxkit_vk::cmd::StencilValue;
use nxkit_vk::info;
use nxkit_vk::mem::Mem;

use super::abi::{M, call, code, handle_out, handles_out, ok, procs};

extern "C" fn create_command_pool(_device: u64, _ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateCommandPool", |vk| {
        handle_out(out, Ok(vk.create_command_pool()))
    })
}

extern "C" fn destroy_command_pool(_device: u64, pool: u64, _alloc: u64) {
    call("vkDestroyCommandPool", |vk| {
        vk.destroy_command_pool(pool);
        Ok(())
    })
}

extern "C" fn reset_command_pool(_device: u64, pool: u64, _flags: u32) -> i32 {
    code("vkResetCommandPool", |vk| ok(vk.reset_command_pool(pool)))
}

extern "C" fn allocate_command_buffers(_device: u64, ai: u64, out: u64) -> i32 {
    code("vkAllocateCommandBuffers", |vk| {
        let (pool, level, count) = info::command_buffer_allocate(&M, ai)?;
        handles_out(out, vk.allocate_command_buffers(pool, level, count))
    })
}

extern "C" fn free_command_buffers(_device: u64, _pool: u64, count: u32, cbs: u64) {
    call("vkFreeCommandBuffers", |vk| {
        vk.free_command_buffers(&info::handles(&M, cbs, count)?);
        Ok(())
    })
}

extern "C" fn begin_command_buffer(cb: u64, _begin: u64) -> i32 {
    code("vkBeginCommandBuffer", |vk| ok(vk.begin_command_buffer(cb)))
}

extern "C" fn end_command_buffer(cb: u64) -> i32 {
    code("vkEndCommandBuffer", |vk| ok(vk.end_command_buffer(cb)))
}

extern "C" fn reset_command_buffer(cb: u64, _flags: u32) -> i32 {
    code("vkResetCommandBuffer", |vk| ok(vk.reset_command_buffer(cb)))
}

extern "C" fn cmd_begin_render_pass(cb: u64, begin: u64, _contents: u32) {
    call("vkCmdBeginRenderPass", |vk| {
        vk.cmd_begin_render_pass(cb, &info::render_pass_begin(&M, begin)?);
        Ok(())
    })
}

extern "C" fn cmd_next_subpass(cb: u64, _contents: u32) {
    call("vkCmdNextSubpass", |vk| {
        vk.cmd_next_subpass(cb);
        Ok(())
    })
}

extern "C" fn cmd_end_render_pass(cb: u64) {
    call("vkCmdEndRenderPass", |vk| {
        vk.cmd_end_render_pass(cb);
        Ok(())
    })
}

extern "C" fn cmd_bind_pipeline(cb: u64, bind_point: u32, pipeline: u64) {
    call("vkCmdBindPipeline", |vk| {
        vk.cmd_bind_pipeline(cb, bind_point, pipeline);
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_bind_descriptor_sets(
    cb: u64,
    bind_point: u32,
    _layout: u64,
    first: u32,
    count: u32,
    sets: u64,
    dynamic_count: u32,
    dynamic: u64,
) {
    call("vkCmdBindDescriptorSets", |vk| {
        let sets = info::handles(&M, sets, count)?;
        let offsets = if dynamic == 0 || dynamic_count == 0 {
            Vec::new()
        } else {
            M.u32s(dynamic, dynamic_count)?
        };
        vk.cmd_bind_descriptor_sets(cb, bind_point, first, &sets, &offsets);
        Ok(())
    })
}

extern "C" fn cmd_bind_vertex_buffers(cb: u64, first: u32, count: u32, buffers: u64, offsets: u64) {
    call("vkCmdBindVertexBuffers", |vk| {
        let b = info::handles(&M, buffers, count)?;
        let o = info::handles(&M, offsets, count)?;
        vk.cmd_bind_vertex_buffers(cb, first, &b, &o);
        Ok(())
    })
}

extern "C" fn cmd_bind_index_buffer(cb: u64, buffer: u64, offset: u64, index_type: u32) {
    call("vkCmdBindIndexBuffer", |vk| {
        vk.cmd_bind_index_buffer(cb, buffer, offset, index_type);
        Ok(())
    })
}

extern "C" fn cmd_set_viewport(cb: u64, first: u32, count: u32, viewports: u64) {
    call("vkCmdSetViewport", |vk| {
        vk.cmd_set_viewport(cb, first, info::viewports(&M, viewports, count)?);
        Ok(())
    })
}

extern "C" fn cmd_set_scissor(cb: u64, first: u32, count: u32, rects: u64) {
    call("vkCmdSetScissor", |vk| {
        vk.cmd_set_scissor(cb, first, info::rects(&M, rects, count)?);
        Ok(())
    })
}

extern "C" fn cmd_set_line_width(cb: u64, width: f32) {
    call("vkCmdSetLineWidth", |vk| {
        vk.cmd_set_line_width(cb, width);
        Ok(())
    })
}

extern "C" fn cmd_set_depth_bias(cb: u64, constant: f32, clamp: f32, slope: f32) {
    call("vkCmdSetDepthBias", |vk| {
        vk.cmd_set_depth_bias(cb, constant, clamp, slope);
        Ok(())
    })
}

/// `vkCmdSetBlendConstants(commandBuffer, const float blendConstants[4])`: the array is a pointer.
extern "C" fn cmd_set_blend_constants(cb: u64, constants: u64) {
    call("vkCmdSetBlendConstants", |vk| {
        let c = [
            M.f32(constants)?,
            M.f32(constants + 4)?,
            M.f32(constants + 8)?,
            M.f32(constants + 12)?,
        ];
        vk.cmd_set_blend_constants(cb, c);
        Ok(())
    })
}

extern "C" fn cmd_set_depth_bounds(cb: u64, min: f32, max: f32) {
    call("vkCmdSetDepthBounds", |vk| {
        vk.cmd_set_depth_bounds(cb, min, max);
        Ok(())
    })
}

extern "C" fn cmd_set_stencil_compare_mask(cb: u64, faces: u32, value: u32) {
    call("vkCmdSetStencilCompareMask", |vk| {
        vk.cmd_set_stencil(cb, StencilValue::CompareMask, faces, value);
        Ok(())
    })
}

extern "C" fn cmd_set_stencil_write_mask(cb: u64, faces: u32, value: u32) {
    call("vkCmdSetStencilWriteMask", |vk| {
        vk.cmd_set_stencil(cb, StencilValue::WriteMask, faces, value);
        Ok(())
    })
}

extern "C" fn cmd_set_stencil_reference(cb: u64, faces: u32, value: u32) {
    call("vkCmdSetStencilReference", |vk| {
        vk.cmd_set_stencil(cb, StencilValue::Reference, faces, value);
        Ok(())
    })
}

/// `vkCmdSetDeviceMask`: one device.
extern "C" fn cmd_set_device_mask(_cb: u64, _mask: u32) {
    call("vkCmdSetDeviceMask", |_| Ok(()))
}

extern "C" fn cmd_push_constants(
    cb: u64,
    _layout: u64,
    stages: u32,
    offset: u32,
    size: u32,
    values: u64,
) {
    call("vkCmdPushConstants", |vk| {
        vk.cmd_push_constants(cb, stages, offset, M.bytes(values, u64::from(size))?);
        Ok(())
    })
}

extern "C" fn cmd_draw(
    cb: u64,
    vertices: u32,
    instances: u32,
    first_vertex: u32,
    first_instance: u32,
) {
    call("vkCmdDraw", |vk| {
        vk.cmd_draw(cb, vertices, instances, first_vertex, first_instance);
        Ok(())
    })
}

extern "C" fn cmd_draw_indexed(
    cb: u64,
    indices: u32,
    instances: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) {
    call("vkCmdDrawIndexed", |vk| {
        vk.cmd_draw_indexed(
            cb,
            indices,
            instances,
            first_index,
            vertex_offset,
            first_instance,
        );
        Ok(())
    })
}

extern "C" fn cmd_draw_indirect(cb: u64, buffer: u64, offset: u64, count: u32, stride: u32) {
    call("vkCmdDrawIndirect", |vk| {
        vk.cmd_draw_indirect(cb, buffer, offset, count, stride, false);
        Ok(())
    })
}

extern "C" fn cmd_draw_indexed_indirect(
    cb: u64,
    buffer: u64,
    offset: u64,
    count: u32,
    stride: u32,
) {
    call("vkCmdDrawIndexedIndirect", |vk| {
        vk.cmd_draw_indirect(cb, buffer, offset, count, stride, true);
        Ok(())
    })
}

/// `vkCmdDrawIndirectCountAMD` and `vkCmdDrawIndexedIndirectCountAMD(commandBuffer, buffer, offset, countBuffer,
/// countBufferOffset, maxDrawCount, stride)`: the extension is not advertised, nothing is recorded.
extern "C" fn cmd_draw_indirect_count(
    cb: u64,
    _buffer: u64,
    _offset: u64,
    _count_buffer: u64,
    _count_offset: u64,
    _max: u32,
    _stride: u32,
) {
    call("vkCmdDrawIndirectCountAMD", |vk| {
        vk.cmd_draw_indirect_count(cb);
        Ok(())
    })
}

extern "C" fn cmd_dispatch(cb: u64, x: u32, y: u32, z: u32) {
    call("vkCmdDispatch", |vk| {
        vk.cmd_dispatch(cb, x, y, z);
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_dispatch_base(cb: u64, bx: u32, by: u32, bz: u32, x: u32, y: u32, z: u32) {
    call("vkCmdDispatchBase", |vk| {
        vk.cmd_dispatch_base(cb, [bx, by, bz], [x, y, z]);
        Ok(())
    })
}

extern "C" fn cmd_dispatch_indirect(cb: u64, buffer: u64, offset: u64) {
    call("vkCmdDispatchIndirect", |vk| {
        vk.cmd_dispatch_indirect(cb, buffer, offset);
        Ok(())
    })
}

pub(super) fn procs() -> Vec<super::Proc> {
    procs![
        "vkCreateCommandPool" => create_command_pool,
        "vkDestroyCommandPool" => destroy_command_pool,
        "vkResetCommandPool" => reset_command_pool,
        "vkAllocateCommandBuffers" => allocate_command_buffers,
        "vkFreeCommandBuffers" => free_command_buffers,
        "vkBeginCommandBuffer" => begin_command_buffer,
        "vkEndCommandBuffer" => end_command_buffer,
        "vkResetCommandBuffer" => reset_command_buffer,
        "vkCmdBeginRenderPass" => cmd_begin_render_pass,
        "vkCmdNextSubpass" => cmd_next_subpass,
        "vkCmdEndRenderPass" => cmd_end_render_pass,
        "vkCmdBindPipeline" => cmd_bind_pipeline,
        "vkCmdBindDescriptorSets" => cmd_bind_descriptor_sets,
        "vkCmdBindVertexBuffers" => cmd_bind_vertex_buffers,
        "vkCmdBindIndexBuffer" => cmd_bind_index_buffer,
        "vkCmdSetViewport" => cmd_set_viewport,
        "vkCmdSetScissor" => cmd_set_scissor,
        "vkCmdSetLineWidth" => cmd_set_line_width,
        "vkCmdSetDepthBias" => cmd_set_depth_bias,
        "vkCmdSetBlendConstants" => cmd_set_blend_constants,
        "vkCmdSetDepthBounds" => cmd_set_depth_bounds,
        "vkCmdSetStencilCompareMask" => cmd_set_stencil_compare_mask,
        "vkCmdSetStencilWriteMask" => cmd_set_stencil_write_mask,
        "vkCmdSetStencilReference" => cmd_set_stencil_reference,
        "vkCmdSetDeviceMask" => cmd_set_device_mask,
        "vkCmdPushConstants" => cmd_push_constants,
        "vkCmdDraw" => cmd_draw,
        "vkCmdDrawIndexed" => cmd_draw_indexed,
        "vkCmdDrawIndirect" => cmd_draw_indirect,
        "vkCmdDrawIndexedIndirect" => cmd_draw_indexed_indirect,
        "vkCmdDrawIndirectCountAMD" => cmd_draw_indirect_count,
        "vkCmdDrawIndexedIndirectCountAMD" => cmd_draw_indirect_count,
        "vkCmdDispatch" => cmd_dispatch,
        "vkCmdDispatchBase" => cmd_dispatch_base,
        "vkCmdDispatchIndirect" => cmd_dispatch_indirect,
    ]
}
