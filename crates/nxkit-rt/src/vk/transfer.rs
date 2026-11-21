//! The recorded commands of copies, blits, resolves, fills, clears, secondary command buffers, queries, events and
//! barriers. Image layouts are passed on where the front end takes them and ignored elsewhere: Metal has none.

use nxkit_vk::info;
use nxkit_vk::mem::Mem;

use super::abi::{M, call, procs};

extern "C" fn cmd_copy_buffer(cb: u64, src: u64, dst: u64, count: u32, regions: u64) {
    call("vkCmdCopyBuffer", |vk| {
        vk.cmd_copy_buffer(cb, src, dst, info::buffer_copies(&M, regions, count)?);
        Ok(())
    })
}

extern "C" fn cmd_copy_buffer_to_image(
    cb: u64,
    src: u64,
    dst: u64,
    _layout: u32,
    count: u32,
    regions: u64,
) {
    call("vkCmdCopyBufferToImage", |vk| {
        vk.cmd_copy_buffer_to_image(cb, src, dst, info::buffer_image_copies(&M, regions, count)?);
        Ok(())
    })
}

extern "C" fn cmd_copy_image_to_buffer(
    cb: u64,
    src: u64,
    _layout: u32,
    dst: u64,
    count: u32,
    regions: u64,
) {
    call("vkCmdCopyImageToBuffer", |vk| {
        vk.cmd_copy_image_to_buffer(cb, src, dst, info::buffer_image_copies(&M, regions, count)?);
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_copy_image(
    cb: u64,
    src: u64,
    _src_layout: u32,
    dst: u64,
    _dst_layout: u32,
    count: u32,
    regions: u64,
) {
    call("vkCmdCopyImage", |vk| {
        vk.cmd_copy_image(cb, src, dst, info::image_copies(&M, regions, count)?);
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_blit_image(
    cb: u64,
    src: u64,
    src_layout: u32,
    dst: u64,
    dst_layout: u32,
    count: u32,
    regions: u64,
    filter: u32,
) {
    call("vkCmdBlitImage", |vk| {
        let r = info::image_blits(&M, regions, count)?;
        vk.cmd_blit_image(cb, src, src_layout, dst, dst_layout, r, filter);
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_resolve_image(
    cb: u64,
    src: u64,
    src_layout: u32,
    dst: u64,
    dst_layout: u32,
    count: u32,
    regions: u64,
) {
    call("vkCmdResolveImage", |vk| {
        let r = info::image_copies(&M, regions, count)?;
        vk.cmd_resolve_image(cb, src, src_layout, dst, dst_layout, r);
        Ok(())
    })
}

extern "C" fn cmd_fill_buffer(cb: u64, dst: u64, offset: u64, size: u64, data: u32) {
    call("vkCmdFillBuffer", |vk| {
        vk.cmd_fill_buffer(cb, dst, offset, size, data);
        Ok(())
    })
}

extern "C" fn cmd_update_buffer(cb: u64, dst: u64, offset: u64, size: u64, data: u64) {
    call("vkCmdUpdateBuffer", |vk| {
        // At most 65536 bytes are valid. Of a larger size one word more is read, so the front end sees that the
        // update is too large, reports it and records nothing, and a garbage size reads no more.
        let bytes = M.bytes(data, size.min(65536 + 4))?;
        vk.cmd_update_buffer(cb, dst, offset, bytes);
        Ok(())
    })
}

extern "C" fn cmd_clear_color_image(
    cb: u64,
    image: u64,
    _layout: u32,
    color: u64,
    count: u32,
    ranges: u64,
) {
    call("vkCmdClearColorImage", |vk| {
        let c = info::clear_value(&M, color)?;
        vk.cmd_clear_color_image(cb, image, c, info::subresource_ranges(&M, ranges, count)?);
        Ok(())
    })
}

/// `VkClearDepthStencilValue`: the depth as a float, then the stencil.
extern "C" fn cmd_clear_depth_stencil_image(
    cb: u64,
    image: u64,
    _layout: u32,
    value: u64,
    count: u32,
    ranges: u64,
) {
    call("vkCmdClearDepthStencilImage", |vk| {
        let (depth, stencil) = (M.f32(value)?, M.u32(value + 4)?);
        let r = info::subresource_ranges(&M, ranges, count)?;
        vk.cmd_clear_depth_stencil_image(cb, image, depth, stencil, r);
        Ok(())
    })
}

extern "C" fn cmd_clear_attachments(
    cb: u64,
    attachment_count: u32,
    attachments: u64,
    rect_count: u32,
    rects: u64,
) {
    call("vkCmdClearAttachments", |vk| {
        let a = info::clear_attachments(&M, attachments, attachment_count)?;
        vk.cmd_clear_attachments(cb, a, info::clear_rects(&M, rects, rect_count)?);
        Ok(())
    })
}

extern "C" fn cmd_execute_commands(cb: u64, count: u32, cbs: u64) {
    call("vkCmdExecuteCommands", |vk| {
        vk.cmd_execute_commands(cb, &info::handles(&M, cbs, count)?);
        Ok(())
    })
}

extern "C" fn cmd_reset_query_pool(cb: u64, pool: u64, first: u32, count: u32) {
    call("vkCmdResetQueryPool", |vk| {
        vk.cmd_reset_query_pool(cb, pool, first, count);
        Ok(())
    })
}

extern "C" fn cmd_begin_query(cb: u64, pool: u64, query: u32, flags: u32) {
    call("vkCmdBeginQuery", |vk| {
        vk.cmd_begin_query(cb, pool, query, flags);
        Ok(())
    })
}

extern "C" fn cmd_end_query(cb: u64, pool: u64, query: u32) {
    call("vkCmdEndQuery", |vk| {
        vk.cmd_end_query(cb, pool, query);
        Ok(())
    })
}

extern "C" fn cmd_write_timestamp(cb: u64, _stage: u32, pool: u64, query: u32) {
    call("vkCmdWriteTimestamp", |vk| {
        vk.cmd_write_timestamp(cb, pool, query);
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_copy_query_pool_results(
    cb: u64,
    pool: u64,
    first: u32,
    count: u32,
    dst: u64,
    offset: u64,
    stride: u64,
    flags: u32,
) {
    call("vkCmdCopyQueryPoolResults", |vk| {
        vk.cmd_copy_query_pool_results(cb, pool, first, count, dst, offset, stride, flags);
        Ok(())
    })
}

extern "C" fn cmd_set_event(cb: u64, event: u64, _stages: u32) {
    call("vkCmdSetEvent", |vk| {
        vk.cmd_set_event(cb, event);
        Ok(())
    })
}

extern "C" fn cmd_reset_event(cb: u64, event: u64, _stages: u32) {
    call("vkCmdResetEvent", |vk| {
        vk.cmd_reset_event(cb, event);
        Ok(())
    })
}

/// `vkCmdWaitEvents`: the one serial queue runs every command in order, so the wait is met.
#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_wait_events(
    cb: u64,
    _event_count: u32,
    _events: u64,
    _src: u32,
    _dst: u32,
    _memory_count: u32,
    _memory: u64,
    _buffer_count: u32,
    _buffers: u64,
    _image_count: u32,
    _images: u64,
) {
    call("vkCmdWaitEvents", |vk| {
        vk.cmd_wait_events(cb);
        Ok(())
    })
}

/// `vkCmdPipelineBarrier`: the front end needs the number of barriers only.
#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_pipeline_barrier(
    cb: u64,
    _src: u32,
    _dst: u32,
    _dependency: u32,
    memory_count: u32,
    _memory: u64,
    buffer_count: u32,
    _buffers: u64,
    image_count: u32,
    _images: u64,
) {
    call("vkCmdPipelineBarrier", |vk| {
        let n = memory_count
            .saturating_add(buffer_count)
            .saturating_add(image_count);
        vk.cmd_pipeline_barrier(cb, n);
        Ok(())
    })
}

/// `vkCmdPipelineBarrier2(commandBuffer, const VkDependencyInfo *)`: the counts at 20, 32 and 48.
extern "C" fn cmd_pipeline_barrier2(cb: u64, dependency: u64) {
    call("vkCmdPipelineBarrier2", |vk| {
        let n = M
            .u32(dependency + 20)?
            .saturating_add(M.u32(dependency + 32)?)
            .saturating_add(M.u32(dependency + 48)?);
        vk.cmd_pipeline_barrier(cb, n);
        Ok(())
    })
}

pub(super) fn procs() -> Vec<super::Proc> {
    procs![
        "vkCmdCopyBuffer" => cmd_copy_buffer,
        "vkCmdCopyBufferToImage" => cmd_copy_buffer_to_image,
        "vkCmdCopyImageToBuffer" => cmd_copy_image_to_buffer,
        "vkCmdCopyImage" => cmd_copy_image,
        "vkCmdBlitImage" => cmd_blit_image,
        "vkCmdResolveImage" => cmd_resolve_image,
        "vkCmdFillBuffer" => cmd_fill_buffer,
        "vkCmdUpdateBuffer" => cmd_update_buffer,
        "vkCmdClearColorImage" => cmd_clear_color_image,
        "vkCmdClearDepthStencilImage" => cmd_clear_depth_stencil_image,
        "vkCmdClearAttachments" => cmd_clear_attachments,
        "vkCmdExecuteCommands" => cmd_execute_commands,
        "vkCmdResetQueryPool" => cmd_reset_query_pool,
        "vkCmdBeginQuery" => cmd_begin_query,
        "vkCmdEndQuery" => cmd_end_query,
        "vkCmdWriteTimestamp" => cmd_write_timestamp,
        "vkCmdCopyQueryPoolResults" => cmd_copy_query_pool_results,
        "vkCmdSetEvent" => cmd_set_event,
        "vkCmdResetEvent" => cmd_reset_event,
        "vkCmdWaitEvents" => cmd_wait_events,
        "vkCmdPipelineBarrier" => cmd_pipeline_barrier,
        "vkCmdPipelineBarrier2" => cmd_pipeline_barrier2,
    ]
}
