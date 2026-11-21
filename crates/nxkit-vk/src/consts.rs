//! The Vulkan enum values and flag bits that the front end reads or reports (values of `vulkan_core.h`).

/// `VkStructureType` values that the front end looks for in `pNext` chains.
pub mod stype {
    /// `VK_STRUCTURE_TYPE_MEMORY_DEDICATED_REQUIREMENTS`.
    pub const MEMORY_DEDICATED_REQUIREMENTS: u32 = 1_000_127_000;
    /// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_GROUP_PROPERTIES`.
    pub const PHYSICAL_DEVICE_GROUP_PROPERTIES: u32 = 1_000_070_000;
}

/// `VkImageType`.
pub mod image_type {
    /// `VK_IMAGE_TYPE_1D`.
    pub const D1: u32 = 0;
    /// `VK_IMAGE_TYPE_2D`.
    pub const D2: u32 = 1;
    /// `VK_IMAGE_TYPE_3D`.
    pub const D3: u32 = 2;
}

/// `VkImageViewType`.
pub mod view_type {
    /// `VK_IMAGE_VIEW_TYPE_1D`.
    pub const D1: u32 = 0;
    /// `VK_IMAGE_VIEW_TYPE_2D`.
    pub const D2: u32 = 1;
    /// `VK_IMAGE_VIEW_TYPE_3D`.
    pub const D3: u32 = 2;
    /// `VK_IMAGE_VIEW_TYPE_CUBE`.
    pub const CUBE: u32 = 3;
    /// `VK_IMAGE_VIEW_TYPE_1D_ARRAY`.
    pub const D1_ARRAY: u32 = 4;
    /// `VK_IMAGE_VIEW_TYPE_2D_ARRAY`.
    pub const D2_ARRAY: u32 = 5;
    /// `VK_IMAGE_VIEW_TYPE_CUBE_ARRAY`.
    pub const CUBE_ARRAY: u32 = 6;
}

/// `VkImageTiling`.
pub mod tiling {
    /// `VK_IMAGE_TILING_OPTIMAL`.
    pub const OPTIMAL: u32 = 0;
    /// `VK_IMAGE_TILING_LINEAR`.
    pub const LINEAR: u32 = 1;
}

/// `VkImageUsageFlagBits`.
pub mod image_usage {
    /// `VK_IMAGE_USAGE_TRANSFER_SRC_BIT`.
    pub const TRANSFER_SRC: u32 = 0x1;
    /// `VK_IMAGE_USAGE_TRANSFER_DST_BIT`.
    pub const TRANSFER_DST: u32 = 0x2;
    /// `VK_IMAGE_USAGE_SAMPLED_BIT`.
    pub const SAMPLED: u32 = 0x4;
    /// `VK_IMAGE_USAGE_STORAGE_BIT`.
    pub const STORAGE: u32 = 0x8;
    /// `VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT`.
    pub const COLOR_ATTACHMENT: u32 = 0x10;
    /// `VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT`.
    pub const DEPTH_STENCIL_ATTACHMENT: u32 = 0x20;
    /// `VK_IMAGE_USAGE_INPUT_ATTACHMENT_BIT`.
    pub const INPUT_ATTACHMENT: u32 = 0x80;
}

/// `VkImageCreateFlagBits`.
pub mod image_flags {
    /// `VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT`.
    pub const MUTABLE_FORMAT: u32 = 0x8;
    /// `VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT`.
    pub const CUBE_COMPATIBLE: u32 = 0x10;
}

/// `VkBufferUsageFlagBits`.
pub mod buffer_usage {
    /// `VK_BUFFER_USAGE_TRANSFER_DST_BIT`.
    pub const TRANSFER_DST: u32 = 0x2;
}

/// `VkMemoryPropertyFlagBits`.
pub mod memory_property {
    /// `VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT`.
    pub const DEVICE_LOCAL: u32 = 0x1;
    /// `VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT`.
    pub const HOST_VISIBLE: u32 = 0x2;
    /// `VK_MEMORY_PROPERTY_HOST_COHERENT_BIT`.
    pub const HOST_COHERENT: u32 = 0x4;
    /// `VK_MEMORY_PROPERTY_HOST_CACHED_BIT`.
    pub const HOST_CACHED: u32 = 0x8;
}

/// `VkFormatFeatureFlagBits`.
pub mod format_feature {
    /// `VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT`.
    pub const SAMPLED_IMAGE: u32 = 0x1;
    /// `VK_FORMAT_FEATURE_STORAGE_IMAGE_BIT`.
    pub const STORAGE_IMAGE: u32 = 0x2;
    /// `VK_FORMAT_FEATURE_UNIFORM_TEXEL_BUFFER_BIT`.
    pub const UNIFORM_TEXEL_BUFFER: u32 = 0x8;
    /// `VK_FORMAT_FEATURE_VERTEX_BUFFER_BIT`.
    pub const VERTEX_BUFFER: u32 = 0x40;
    /// `VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT`.
    pub const COLOR_ATTACHMENT: u32 = 0x80;
    /// `VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BLEND_BIT`.
    pub const COLOR_ATTACHMENT_BLEND: u32 = 0x100;
    /// `VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT`.
    pub const DEPTH_STENCIL_ATTACHMENT: u32 = 0x200;
    /// `VK_FORMAT_FEATURE_BLIT_SRC_BIT`.
    pub const BLIT_SRC: u32 = 0x400;
    /// `VK_FORMAT_FEATURE_BLIT_DST_BIT`.
    pub const BLIT_DST: u32 = 0x800;
    /// `VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT`.
    pub const SAMPLED_IMAGE_FILTER_LINEAR: u32 = 0x1000;
    /// `VK_FORMAT_FEATURE_TRANSFER_SRC_BIT`.
    pub const TRANSFER_SRC: u32 = 0x4000;
    /// `VK_FORMAT_FEATURE_TRANSFER_DST_BIT`.
    pub const TRANSFER_DST: u32 = 0x8000;
}

/// `VkDescriptorType`.
pub mod descriptor_type {
    /// `VK_DESCRIPTOR_TYPE_SAMPLER`.
    pub const SAMPLER: u32 = 0;
    /// `VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER`.
    pub const COMBINED_IMAGE_SAMPLER: u32 = 1;
    /// `VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE`.
    pub const SAMPLED_IMAGE: u32 = 2;
    /// `VK_DESCRIPTOR_TYPE_STORAGE_IMAGE`.
    pub const STORAGE_IMAGE: u32 = 3;
    /// `VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER`.
    pub const UNIFORM_TEXEL_BUFFER: u32 = 4;
    /// `VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER`.
    pub const STORAGE_TEXEL_BUFFER: u32 = 5;
    /// `VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER`.
    pub const UNIFORM_BUFFER: u32 = 6;
    /// `VK_DESCRIPTOR_TYPE_STORAGE_BUFFER`.
    pub const STORAGE_BUFFER: u32 = 7;
    /// `VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC`.
    pub const UNIFORM_BUFFER_DYNAMIC: u32 = 8;
    /// `VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC`.
    pub const STORAGE_BUFFER_DYNAMIC: u32 = 9;
    /// `VK_DESCRIPTOR_TYPE_INPUT_ATTACHMENT`.
    pub const INPUT_ATTACHMENT: u32 = 10;
}

/// `VkShaderStageFlagBits`.
pub mod stage {
    /// `VK_SHADER_STAGE_VERTEX_BIT`.
    pub const VERTEX: u32 = 0x1;
    /// `VK_SHADER_STAGE_GEOMETRY_BIT`.
    pub const GEOMETRY: u32 = 0x8;
    /// `VK_SHADER_STAGE_FRAGMENT_BIT`.
    pub const FRAGMENT: u32 = 0x10;
    /// `VK_SHADER_STAGE_COMPUTE_BIT`.
    pub const COMPUTE: u32 = 0x20;
}

/// `VkDynamicState`.
pub mod dynamic_state {
    /// `VK_DYNAMIC_STATE_VIEWPORT`.
    pub const VIEWPORT: u32 = 0;
    /// `VK_DYNAMIC_STATE_SCISSOR`.
    pub const SCISSOR: u32 = 1;
    /// `VK_DYNAMIC_STATE_LINE_WIDTH`.
    pub const LINE_WIDTH: u32 = 2;
    /// `VK_DYNAMIC_STATE_DEPTH_BIAS`.
    pub const DEPTH_BIAS: u32 = 3;
    /// `VK_DYNAMIC_STATE_BLEND_CONSTANTS`.
    pub const BLEND_CONSTANTS: u32 = 4;
    /// `VK_DYNAMIC_STATE_DEPTH_BOUNDS`.
    pub const DEPTH_BOUNDS: u32 = 5;
    /// `VK_DYNAMIC_STATE_STENCIL_COMPARE_MASK`.
    pub const STENCIL_COMPARE_MASK: u32 = 6;
    /// `VK_DYNAMIC_STATE_STENCIL_WRITE_MASK`.
    pub const STENCIL_WRITE_MASK: u32 = 7;
    /// `VK_DYNAMIC_STATE_STENCIL_REFERENCE`.
    pub const STENCIL_REFERENCE: u32 = 8;
}

/// `VkPipelineBindPoint`.
pub mod bind_point {
    /// `VK_PIPELINE_BIND_POINT_GRAPHICS`.
    pub const GRAPHICS: u32 = 0;
    /// `VK_PIPELINE_BIND_POINT_COMPUTE`.
    pub const COMPUTE: u32 = 1;
}

/// `VkQueryType`.
pub mod query_type {
    /// `VK_QUERY_TYPE_OCCLUSION`.
    pub const OCCLUSION: u32 = 0;
    /// `VK_QUERY_TYPE_TIMESTAMP`.
    pub const TIMESTAMP: u32 = 2;
}

/// `VkQueryResultFlagBits`.
pub mod query_result {
    /// `VK_QUERY_RESULT_64_BIT`.
    pub const R64: u32 = 0x1;
    /// `VK_QUERY_RESULT_WAIT_BIT`.
    pub const WAIT: u32 = 0x2;
    /// `VK_QUERY_RESULT_WITH_AVAILABILITY_BIT`.
    pub const WITH_AVAILABILITY: u32 = 0x4;
}

/// Special values of counts and indices.
pub mod special {
    /// `VK_ATTACHMENT_UNUSED`.
    pub const ATTACHMENT_UNUSED: u32 = u32::MAX;
    /// `VK_REMAINING_MIP_LEVELS` and `VK_REMAINING_ARRAY_LAYERS`.
    pub const REMAINING: u32 = u32::MAX;
    /// `VK_WHOLE_SIZE`.
    pub const WHOLE_SIZE: u64 = u64::MAX;
    /// `VK_FENCE_CREATE_SIGNALED_BIT`.
    pub const FENCE_SIGNALED: u32 = 0x1;
    /// `VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET`.
    pub const TEMPLATE_DESCRIPTOR_SET: u32 = 0;
    /// `VK_COMMAND_BUFFER_LEVEL_SECONDARY`.
    pub const LEVEL_SECONDARY: u32 = 1;
}

/// Makes a `VK_MAKE_API_VERSION` value.
pub const fn api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
    variant << 29 | major << 22 | minor << 12 | patch
}
