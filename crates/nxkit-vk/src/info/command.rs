//! Decoders of the structs of command pools and buffers, recorded commands, sync objects, submits and the swapchain.

use crate::cmd::{
    BufferCopy, BufferImageCopy, ClearAttachment, ClearRect, ClearValue, ImageBlit, ImageCopy,
    SubresourceLayers,
};
use crate::desc::Rect2D;
use crate::layout::{
    buffer_copy as bcp, buffer_image_copy as bic, clear_attachment as ca, clear_rect as cr,
    command_buffer_allocate_info as cba, image_blit as ib, image_copy as icp,
    image_subresource_layers as isl, image_subresource_range as isr, present_info_khr as pi,
    query_pool_create_info as qpc, render_pass_begin_info as rpb, submit_info as si,
    swapchain_create_info_khr as scc,
};
use crate::mem::Mem;
use crate::result::VkResult;

use super::{SubresourceRange, array, extent3, handles, offset3, rect, subresource_range};

/// Reads a `VkCommandBufferAllocateInfo`: the pool, the level and the count.
pub fn command_buffer_allocate(m: &impl Mem, at: u64) -> VkResult<(u64, u32, u32)> {
    Ok((
        m.u64(at + cba::COMMAND_POOL)?,
        m.u32(at + cba::LEVEL)?,
        m.u32(at + cba::COMMAND_BUFFER_COUNT)?,
    ))
}

/// Reads the `flags` at offset 16 of a create info (`VkFenceCreateInfo`, `VkCommandPoolCreateInfo`, ...). A null
/// struct reads as zero flags.
pub fn flags(m: &impl Mem, at: u64) -> VkResult<u32> {
    if at == 0 {
        return Ok(0);
    }
    m.u32(at + 16)
}

/// Reads a `VkQueryPoolCreateInfo`: the query type and the count.
pub fn query_pool(m: &impl Mem, at: u64) -> VkResult<(u32, u32)> {
    Ok((m.u32(at + qpc::QUERY_TYPE)?, m.u32(at + qpc::QUERY_COUNT)?))
}

/// A `VkRenderPassBeginInfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderPassBeginInfo {
    /// `renderPass`.
    pub render_pass: u64,
    /// `framebuffer`.
    pub framebuffer: u64,
    /// `renderArea`.
    pub area: Rect2D,
    /// The clear values, at most 16.
    pub clear: Vec<ClearValue>,
}

/// Reads a `VkClearValue`.
pub fn clear_value(m: &impl Mem, at: u64) -> VkResult<ClearValue> {
    Ok([m.u32(at)?, m.u32(at + 4)?, m.u32(at + 8)?, m.u32(at + 12)?])
}

/// Reads a `VkRenderPassBeginInfo`.
pub fn render_pass_begin(m: &impl Mem, at: u64) -> VkResult<RenderPassBeginInfo> {
    let n = m.u32(at + rpb::CLEAR_VALUE_COUNT)?.min(16);
    Ok(RenderPassBeginInfo {
        render_pass: m.u64(at + rpb::RENDER_PASS)?,
        framebuffer: m.u64(at + rpb::FRAMEBUFFER)?,
        area: rect(m, at + rpb::RENDER_AREA)?,
        clear: array(m, m.u64(at + rpb::P_CLEAR_VALUES)?, n, 16, |m, a| {
            clear_value(m, a)
        })?,
    })
}

fn layers(m: &impl Mem, at: u64) -> VkResult<SubresourceLayers> {
    Ok(SubresourceLayers {
        aspect: m.u32(at + isl::ASPECT_MASK)?,
        level: m.u32(at + isl::MIP_LEVEL)?,
        base_layer: m.u32(at + isl::BASE_ARRAY_LAYER)?,
        layers: m.u32(at + isl::LAYER_COUNT)?,
    })
}

/// Reads `n` `VkBufferCopy`.
pub fn buffer_copies(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<BufferCopy>> {
    array(m, at, n, bcp::SIZE, |m, a| {
        Ok(BufferCopy {
            src_offset: m.u64(a + bcp::SRC_OFFSET)?,
            dst_offset: m.u64(a + bcp::DST_OFFSET)?,
            size: m.u64(a + bcp::SIZE_)?,
        })
    })
}

/// Reads `n` `VkBufferImageCopy`.
pub fn buffer_image_copies(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<BufferImageCopy>> {
    array(m, at, n, bic::SIZE, |m, a| {
        Ok(BufferImageCopy {
            buffer_offset: m.u64(a + bic::BUFFER_OFFSET)?,
            row_length: m.u32(a + bic::BUFFER_ROW_LENGTH)?,
            image_height: m.u32(a + bic::BUFFER_IMAGE_HEIGHT)?,
            sub: layers(m, a + bic::IMAGE_SUBRESOURCE)?,
            offset: offset3(m, a + bic::IMAGE_OFFSET)?,
            extent: extent3(m, a + bic::IMAGE_EXTENT)?,
        })
    })
}

/// Reads `n` `VkImageCopy` (also `VkImageResolve`, which has the same layout).
pub fn image_copies(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<ImageCopy>> {
    array(m, at, n, icp::SIZE, |m, a| {
        Ok(ImageCopy {
            src_sub: layers(m, a + icp::SRC_SUBRESOURCE)?,
            src_offset: offset3(m, a + icp::SRC_OFFSET)?,
            dst_sub: layers(m, a + icp::DST_SUBRESOURCE)?,
            dst_offset: offset3(m, a + icp::DST_OFFSET)?,
            extent: extent3(m, a + icp::EXTENT)?,
        })
    })
}

/// Reads `n` `VkImageBlit`.
pub fn image_blits(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<ImageBlit>> {
    array(m, at, n, ib::SIZE, |m, a| {
        Ok(ImageBlit {
            src_sub: layers(m, a + ib::SRC_SUBRESOURCE)?,
            src_offsets: [
                offset3(m, a + ib::SRC_OFFSETS)?,
                offset3(m, a + ib::SRC_OFFSETS + 12)?,
            ],
            dst_sub: layers(m, a + ib::DST_SUBRESOURCE)?,
            dst_offsets: [
                offset3(m, a + ib::DST_OFFSETS)?,
                offset3(m, a + ib::DST_OFFSETS + 12)?,
            ],
        })
    })
}

/// Reads `n` `VkImageSubresourceRange`.
pub fn subresource_ranges(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<SubresourceRange>> {
    array(m, at, n, isr::SIZE, |m, a| subresource_range(m, a))
}

/// Reads `n` `VkClearAttachment`.
pub fn clear_attachments(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<ClearAttachment>> {
    array(m, at, n, ca::SIZE, |m, a| {
        Ok(ClearAttachment {
            aspect: m.u32(a + ca::ASPECT_MASK)?,
            color_attachment: m.u32(a + ca::COLOR_ATTACHMENT)?,
            value: clear_value(m, a + ca::CLEAR_VALUE)?,
        })
    })
}

/// Reads `n` `VkClearRect`.
pub fn clear_rects(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<ClearRect>> {
    array(m, at, n, cr::SIZE, |m, a| {
        Ok(ClearRect {
            rect: rect(m, a + cr::RECT)?,
            base_layer: m.u32(a + cr::BASE_ARRAY_LAYER)?,
            layers: m.u32(a + cr::LAYER_COUNT)?,
        })
    })
}

/// Reads the command buffers of `n` `VkSubmitInfo`, one list per submit info. Semaphores are not read: one serial
/// queue runs every submit in order, so every wait is met.
pub fn submits(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<Vec<u64>>> {
    array(m, at, n, si::SIZE, |m, a| {
        handles(
            m,
            m.u64(a + si::P_COMMAND_BUFFERS)?,
            m.u32(a + si::COMMAND_BUFFER_COUNT)?,
        )
    })
}

/// A `VkSwapchainCreateInfoKHR`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapchainInfo {
    /// `surface`.
    pub surface: u64,
    /// `minImageCount`.
    pub min_images: u32,
    /// `imageFormat`.
    pub format: u32,
    /// `imageExtent`.
    pub extent: (u32, u32),
    /// `imageUsage`.
    pub usage: u32,
    /// `presentMode`.
    pub present_mode: u32,
    /// `oldSwapchain`.
    pub old: u64,
}

/// Reads a `VkSwapchainCreateInfoKHR`.
pub fn swapchain(m: &impl Mem, at: u64) -> VkResult<SwapchainInfo> {
    Ok(SwapchainInfo {
        surface: m.u64(at + scc::SURFACE)?,
        min_images: m.u32(at + scc::MIN_IMAGE_COUNT)?,
        format: m.u32(at + scc::IMAGE_FORMAT)?,
        extent: (
            m.u32(at + scc::IMAGE_EXTENT)?,
            m.u32(at + scc::IMAGE_EXTENT + 4)?,
        ),
        usage: m.u32(at + scc::IMAGE_USAGE)?,
        present_mode: m.u32(at + scc::PRESENT_MODE)?,
        old: m.u64(at + scc::OLD_SWAPCHAIN)?,
    })
}

/// A `VkPresentInfoKHR`: the swapchains with their image indices, and where the per-swapchain results go (0 for
/// none).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PresentInfo {
    /// The swapchains and image indices.
    pub images: Vec<(u64, u32)>,
    /// `pResults`.
    pub results: u64,
}

/// Reads a `VkPresentInfoKHR`.
pub fn present(m: &impl Mem, at: u64) -> VkResult<PresentInfo> {
    let n = m.u32(at + pi::SWAPCHAIN_COUNT)?;
    let chains = handles(m, m.u64(at + pi::P_SWAPCHAINS)?, n)?;
    let idx = m.u64(at + pi::P_IMAGE_INDICES)?;
    let indices = if idx == 0 || n == 0 {
        Vec::new()
    } else {
        m.u32s(idx, n)?
    };
    Ok(PresentInfo {
        images: chains.into_iter().zip(indices).collect(),
        results: m.u64(at + pi::P_RESULTS)?,
    })
}

/// The `window` of a `VkViSurfaceCreateInfoNN` (`sType`, `pNext`, `flags`, then the window at 24).
pub fn vi_surface_window(m: &impl Mem, at: u64) -> VkResult<u64> {
    m.u64(at + 24)
}
