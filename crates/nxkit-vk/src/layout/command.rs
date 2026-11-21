//! Field offsets and sizes of the Vulkan structs for command pools and buffers, recorded commands, sync objects, submits and the swapchain (64-bit layout).

/// `VkCommandPoolCreateInfo`.
pub(crate) mod command_pool_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const QUEUE_FAMILY_INDEX: u64 = 20;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkCommandBufferAllocateInfo`.
pub(crate) mod command_buffer_allocate_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const COMMAND_POOL: u64 = 16;
    pub(crate) const LEVEL: u64 = 24;
    pub(crate) const COMMAND_BUFFER_COUNT: u64 = 28;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkCommandBufferBeginInfo`.
pub(crate) mod command_buffer_begin_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const P_INHERITANCE_INFO: u64 = 24;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkRenderPassBeginInfo`.
pub(crate) mod render_pass_begin_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const RENDER_PASS: u64 = 16;
    pub(crate) const FRAMEBUFFER: u64 = 24;
    pub(crate) const RENDER_AREA: u64 = 32;
    pub(crate) const CLEAR_VALUE_COUNT: u64 = 48;
    pub(crate) const P_CLEAR_VALUES: u64 = 56;
    pub(crate) const SIZE: u64 = 64;
}

/// `VkBufferCopy`.
pub(crate) mod buffer_copy {
    pub(crate) const SRC_OFFSET: u64 = 0;
    pub(crate) const DST_OFFSET: u64 = 8;
    pub(crate) const SIZE_: u64 = 16;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkImageSubresourceLayers`.
pub(crate) mod image_subresource_layers {
    pub(crate) const ASPECT_MASK: u64 = 0;
    pub(crate) const MIP_LEVEL: u64 = 4;
    pub(crate) const BASE_ARRAY_LAYER: u64 = 8;
    pub(crate) const LAYER_COUNT: u64 = 12;
    pub(crate) const SIZE: u64 = 16;
}

/// `VkBufferImageCopy`.
pub(crate) mod buffer_image_copy {
    pub(crate) const BUFFER_OFFSET: u64 = 0;
    pub(crate) const BUFFER_ROW_LENGTH: u64 = 8;
    pub(crate) const BUFFER_IMAGE_HEIGHT: u64 = 12;
    pub(crate) const IMAGE_SUBRESOURCE: u64 = 16;
    pub(crate) const IMAGE_OFFSET: u64 = 32;
    pub(crate) const IMAGE_EXTENT: u64 = 44;
    pub(crate) const SIZE: u64 = 56;
}

/// `VkImageCopy`.
pub(crate) mod image_copy {
    pub(crate) const SRC_SUBRESOURCE: u64 = 0;
    pub(crate) const SRC_OFFSET: u64 = 16;
    pub(crate) const DST_SUBRESOURCE: u64 = 28;
    pub(crate) const DST_OFFSET: u64 = 44;
    pub(crate) const EXTENT: u64 = 56;
    pub(crate) const SIZE: u64 = 68;
}

/// `VkImageBlit`.
pub(crate) mod image_blit {
    pub(crate) const SRC_SUBRESOURCE: u64 = 0;
    pub(crate) const SRC_OFFSETS: u64 = 16;
    pub(crate) const DST_SUBRESOURCE: u64 = 40;
    pub(crate) const DST_OFFSETS: u64 = 56;
    pub(crate) const SIZE: u64 = 80;
}

/// `VkImageResolve`.
pub(crate) mod image_resolve {
    pub(crate) const SRC_SUBRESOURCE: u64 = 0;
    pub(crate) const SRC_OFFSET: u64 = 16;
    pub(crate) const DST_SUBRESOURCE: u64 = 28;
    pub(crate) const DST_OFFSET: u64 = 44;
    pub(crate) const EXTENT: u64 = 56;
    pub(crate) const SIZE: u64 = 68;
}

/// `VkClearDepthStencilValue`.
pub(crate) mod clear_depth_stencil_value {
    pub(crate) const DEPTH: u64 = 0;
    pub(crate) const STENCIL: u64 = 4;
    pub(crate) const SIZE: u64 = 8;
}

/// `VkClearAttachment`.
pub(crate) mod clear_attachment {
    pub(crate) const ASPECT_MASK: u64 = 0;
    pub(crate) const COLOR_ATTACHMENT: u64 = 4;
    pub(crate) const CLEAR_VALUE: u64 = 8;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkClearRect`.
pub(crate) mod clear_rect {
    pub(crate) const RECT: u64 = 0;
    pub(crate) const BASE_ARRAY_LAYER: u64 = 16;
    pub(crate) const LAYER_COUNT: u64 = 20;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkFenceCreateInfo`.
pub(crate) mod fence_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkQueryPoolCreateInfo`.
pub(crate) mod query_pool_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const QUERY_TYPE: u64 = 20;
    pub(crate) const QUERY_COUNT: u64 = 24;
    pub(crate) const PIPELINE_STATISTICS: u64 = 28;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkSubmitInfo`.
pub(crate) mod submit_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const WAIT_SEMAPHORE_COUNT: u64 = 16;
    pub(crate) const P_WAIT_SEMAPHORES: u64 = 24;
    pub(crate) const P_WAIT_DST_STAGE_MASK: u64 = 32;
    pub(crate) const COMMAND_BUFFER_COUNT: u64 = 40;
    pub(crate) const P_COMMAND_BUFFERS: u64 = 48;
    pub(crate) const SIGNAL_SEMAPHORE_COUNT: u64 = 56;
    pub(crate) const P_SIGNAL_SEMAPHORES: u64 = 64;
    pub(crate) const SIZE: u64 = 72;
}

/// `VkSwapchainCreateInfoKHR`.
pub(crate) mod swapchain_create_info_khr {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const SURFACE: u64 = 24;
    pub(crate) const MIN_IMAGE_COUNT: u64 = 32;
    pub(crate) const IMAGE_FORMAT: u64 = 36;
    pub(crate) const IMAGE_COLOR_SPACE: u64 = 40;
    pub(crate) const IMAGE_EXTENT: u64 = 44;
    pub(crate) const IMAGE_ARRAY_LAYERS: u64 = 52;
    pub(crate) const IMAGE_USAGE: u64 = 56;
    pub(crate) const IMAGE_SHARING_MODE: u64 = 60;
    pub(crate) const QUEUE_FAMILY_INDEX_COUNT: u64 = 64;
    pub(crate) const P_QUEUE_FAMILY_INDICES: u64 = 72;
    pub(crate) const PRE_TRANSFORM: u64 = 80;
    pub(crate) const COMPOSITE_ALPHA: u64 = 84;
    pub(crate) const PRESENT_MODE: u64 = 88;
    pub(crate) const CLIPPED: u64 = 92;
    pub(crate) const OLD_SWAPCHAIN: u64 = 96;
    pub(crate) const SIZE: u64 = 104;
}

/// `VkPresentInfoKHR`.
pub(crate) mod present_info_khr {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const WAIT_SEMAPHORE_COUNT: u64 = 16;
    pub(crate) const P_WAIT_SEMAPHORES: u64 = 24;
    pub(crate) const SWAPCHAIN_COUNT: u64 = 32;
    pub(crate) const P_SWAPCHAINS: u64 = 40;
    pub(crate) const P_IMAGE_INDICES: u64 = 48;
    pub(crate) const P_RESULTS: u64 = 56;
    pub(crate) const SIZE: u64 = 64;
}

/// `VkImageSubresource`.
pub(crate) mod image_subresource {
    pub(crate) const ASPECT_MASK: u64 = 0;
    pub(crate) const MIP_LEVEL: u64 = 4;
    pub(crate) const ARRAY_LAYER: u64 = 8;
    pub(crate) const SIZE: u64 = 12;
}
