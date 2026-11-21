//! Field offsets and sizes of the Vulkan structs for render passes, framebuffers, shader modules and pipelines (64-bit layout).

/// `VkAttachmentDescription`.
pub(crate) mod attachment_description {
    pub(crate) const FLAGS: u64 = 0;
    pub(crate) const FORMAT: u64 = 4;
    pub(crate) const SAMPLES: u64 = 8;
    pub(crate) const LOAD_OP: u64 = 12;
    pub(crate) const STORE_OP: u64 = 16;
    pub(crate) const STENCIL_LOAD_OP: u64 = 20;
    pub(crate) const STENCIL_STORE_OP: u64 = 24;
    pub(crate) const INITIAL_LAYOUT: u64 = 28;
    pub(crate) const FINAL_LAYOUT: u64 = 32;
    pub(crate) const SIZE: u64 = 36;
}

/// `VkAttachmentReference`.
pub(crate) mod attachment_reference {
    pub(crate) const ATTACHMENT: u64 = 0;
    pub(crate) const LAYOUT: u64 = 4;
    pub(crate) const SIZE: u64 = 8;
}

/// `VkSubpassDescription`.
pub(crate) mod subpass_description {
    pub(crate) const FLAGS: u64 = 0;
    pub(crate) const PIPELINE_BIND_POINT: u64 = 4;
    pub(crate) const INPUT_ATTACHMENT_COUNT: u64 = 8;
    pub(crate) const P_INPUT_ATTACHMENTS: u64 = 16;
    pub(crate) const COLOR_ATTACHMENT_COUNT: u64 = 24;
    pub(crate) const P_COLOR_ATTACHMENTS: u64 = 32;
    pub(crate) const P_RESOLVE_ATTACHMENTS: u64 = 40;
    pub(crate) const P_DEPTH_STENCIL_ATTACHMENT: u64 = 48;
    pub(crate) const PRESERVE_ATTACHMENT_COUNT: u64 = 56;
    pub(crate) const P_PRESERVE_ATTACHMENTS: u64 = 64;
    pub(crate) const SIZE: u64 = 72;
}

/// `VkRenderPassCreateInfo`.
pub(crate) mod render_pass_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const ATTACHMENT_COUNT: u64 = 20;
    pub(crate) const P_ATTACHMENTS: u64 = 24;
    pub(crate) const SUBPASS_COUNT: u64 = 32;
    pub(crate) const P_SUBPASSES: u64 = 40;
    pub(crate) const DEPENDENCY_COUNT: u64 = 48;
    pub(crate) const P_DEPENDENCIES: u64 = 56;
    pub(crate) const SIZE: u64 = 64;
}

/// `VkFramebufferCreateInfo`.
pub(crate) mod framebuffer_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const RENDER_PASS: u64 = 24;
    pub(crate) const ATTACHMENT_COUNT: u64 = 32;
    pub(crate) const P_ATTACHMENTS: u64 = 40;
    pub(crate) const WIDTH: u64 = 48;
    pub(crate) const HEIGHT: u64 = 52;
    pub(crate) const LAYERS: u64 = 56;
    pub(crate) const SIZE: u64 = 64;
}

/// `VkShaderModuleCreateInfo`.
pub(crate) mod shader_module_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const CODE_SIZE: u64 = 24;
    pub(crate) const P_CODE: u64 = 32;
    pub(crate) const SIZE: u64 = 40;
}

/// `VkSpecializationMapEntry`.
pub(crate) mod specialization_map_entry {
    pub(crate) const CONSTANT_ID: u64 = 0;
    pub(crate) const OFFSET: u64 = 4;
    pub(crate) const SIZE_: u64 = 8;
    pub(crate) const SIZE: u64 = 16;
}

/// `VkSpecializationInfo`.
pub(crate) mod specialization_info {
    pub(crate) const MAP_ENTRY_COUNT: u64 = 0;
    pub(crate) const P_MAP_ENTRIES: u64 = 8;
    pub(crate) const DATA_SIZE: u64 = 16;
    pub(crate) const P_DATA: u64 = 24;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkPipelineShaderStageCreateInfo`.
pub(crate) mod pipeline_shader_stage_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const STAGE: u64 = 20;
    pub(crate) const MODULE: u64 = 24;
    pub(crate) const P_NAME: u64 = 32;
    pub(crate) const P_SPECIALIZATION_INFO: u64 = 40;
    pub(crate) const SIZE: u64 = 48;
}

/// `VkVertexInputBindingDescription`.
pub(crate) mod vertex_input_binding_description {
    pub(crate) const BINDING: u64 = 0;
    pub(crate) const STRIDE: u64 = 4;
    pub(crate) const INPUT_RATE: u64 = 8;
    pub(crate) const SIZE: u64 = 12;
}

/// `VkVertexInputAttributeDescription`.
pub(crate) mod vertex_input_attribute_description {
    pub(crate) const LOCATION: u64 = 0;
    pub(crate) const BINDING: u64 = 4;
    pub(crate) const FORMAT: u64 = 8;
    pub(crate) const OFFSET: u64 = 12;
    pub(crate) const SIZE: u64 = 16;
}

/// `VkPipelineVertexInputStateCreateInfo`.
pub(crate) mod pipeline_vertex_input_state_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const VERTEX_BINDING_DESCRIPTION_COUNT: u64 = 20;
    pub(crate) const P_VERTEX_BINDING_DESCRIPTIONS: u64 = 24;
    pub(crate) const VERTEX_ATTRIBUTE_DESCRIPTION_COUNT: u64 = 32;
    pub(crate) const P_VERTEX_ATTRIBUTE_DESCRIPTIONS: u64 = 40;
    pub(crate) const SIZE: u64 = 48;
}

/// `VkPipelineInputAssemblyStateCreateInfo`.
pub(crate) mod pipeline_input_assembly_state_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const TOPOLOGY: u64 = 20;
    pub(crate) const PRIMITIVE_RESTART_ENABLE: u64 = 24;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkViewport`.
pub(crate) mod viewport {
    pub(crate) const X: u64 = 0;
    pub(crate) const Y: u64 = 4;
    pub(crate) const WIDTH: u64 = 8;
    pub(crate) const HEIGHT: u64 = 12;
    pub(crate) const MIN_DEPTH: u64 = 16;
    pub(crate) const MAX_DEPTH: u64 = 20;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkRect2D`.
pub(crate) mod rect2d {
    pub(crate) const OFFSET: u64 = 0;
    pub(crate) const EXTENT: u64 = 8;
    pub(crate) const SIZE: u64 = 16;
}

/// `VkPipelineViewportStateCreateInfo`.
pub(crate) mod pipeline_viewport_state_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const VIEWPORT_COUNT: u64 = 20;
    pub(crate) const P_VIEWPORTS: u64 = 24;
    pub(crate) const SCISSOR_COUNT: u64 = 32;
    pub(crate) const P_SCISSORS: u64 = 40;
    pub(crate) const SIZE: u64 = 48;
}

/// `VkPipelineRasterizationStateCreateInfo`.
pub(crate) mod pipeline_rasterization_state_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const DEPTH_CLAMP_ENABLE: u64 = 20;
    pub(crate) const RASTERIZER_DISCARD_ENABLE: u64 = 24;
    pub(crate) const POLYGON_MODE: u64 = 28;
    pub(crate) const CULL_MODE: u64 = 32;
    pub(crate) const FRONT_FACE: u64 = 36;
    pub(crate) const DEPTH_BIAS_ENABLE: u64 = 40;
    pub(crate) const DEPTH_BIAS_CONSTANT_FACTOR: u64 = 44;
    pub(crate) const DEPTH_BIAS_CLAMP: u64 = 48;
    pub(crate) const DEPTH_BIAS_SLOPE_FACTOR: u64 = 52;
    pub(crate) const LINE_WIDTH: u64 = 56;
    pub(crate) const SIZE: u64 = 64;
}

/// `VkPipelineMultisampleStateCreateInfo`.
pub(crate) mod pipeline_multisample_state_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const RASTERIZATION_SAMPLES: u64 = 20;
    pub(crate) const SAMPLE_SHADING_ENABLE: u64 = 24;
    pub(crate) const MIN_SAMPLE_SHADING: u64 = 28;
    pub(crate) const P_SAMPLE_MASK: u64 = 32;
    pub(crate) const ALPHA_TO_COVERAGE_ENABLE: u64 = 40;
    pub(crate) const ALPHA_TO_ONE_ENABLE: u64 = 44;
    pub(crate) const SIZE: u64 = 48;
}

/// `VkStencilOpState`.
pub(crate) mod stencil_op_state {
    pub(crate) const FAIL_OP: u64 = 0;
    pub(crate) const PASS_OP: u64 = 4;
    pub(crate) const DEPTH_FAIL_OP: u64 = 8;
    pub(crate) const COMPARE_OP: u64 = 12;
    pub(crate) const COMPARE_MASK: u64 = 16;
    pub(crate) const WRITE_MASK: u64 = 20;
    pub(crate) const REFERENCE: u64 = 24;
    pub(crate) const SIZE: u64 = 28;
}

/// `VkPipelineDepthStencilStateCreateInfo`.
pub(crate) mod pipeline_depth_stencil_state_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const DEPTH_TEST_ENABLE: u64 = 20;
    pub(crate) const DEPTH_WRITE_ENABLE: u64 = 24;
    pub(crate) const DEPTH_COMPARE_OP: u64 = 28;
    pub(crate) const DEPTH_BOUNDS_TEST_ENABLE: u64 = 32;
    pub(crate) const STENCIL_TEST_ENABLE: u64 = 36;
    pub(crate) const FRONT: u64 = 40;
    pub(crate) const BACK: u64 = 68;
    pub(crate) const MIN_DEPTH_BOUNDS: u64 = 96;
    pub(crate) const MAX_DEPTH_BOUNDS: u64 = 100;
    pub(crate) const SIZE: u64 = 104;
}

/// `VkPipelineColorBlendAttachmentState`.
pub(crate) mod pipeline_color_blend_attachment_state {
    pub(crate) const BLEND_ENABLE: u64 = 0;
    pub(crate) const SRC_COLOR_BLEND_FACTOR: u64 = 4;
    pub(crate) const DST_COLOR_BLEND_FACTOR: u64 = 8;
    pub(crate) const COLOR_BLEND_OP: u64 = 12;
    pub(crate) const SRC_ALPHA_BLEND_FACTOR: u64 = 16;
    pub(crate) const DST_ALPHA_BLEND_FACTOR: u64 = 20;
    pub(crate) const ALPHA_BLEND_OP: u64 = 24;
    pub(crate) const COLOR_WRITE_MASK: u64 = 28;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkPipelineColorBlendStateCreateInfo`.
pub(crate) mod pipeline_color_blend_state_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const LOGIC_OP_ENABLE: u64 = 20;
    pub(crate) const LOGIC_OP: u64 = 24;
    pub(crate) const ATTACHMENT_COUNT: u64 = 28;
    pub(crate) const P_ATTACHMENTS: u64 = 32;
    pub(crate) const BLEND_CONSTANTS: u64 = 40;
    pub(crate) const SIZE: u64 = 56;
}

/// `VkPipelineDynamicStateCreateInfo`.
pub(crate) mod pipeline_dynamic_state_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const DYNAMIC_STATE_COUNT: u64 = 20;
    pub(crate) const P_DYNAMIC_STATES: u64 = 24;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkGraphicsPipelineCreateInfo`.
pub(crate) mod graphics_pipeline_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const STAGE_COUNT: u64 = 20;
    pub(crate) const P_STAGES: u64 = 24;
    pub(crate) const P_VERTEX_INPUT_STATE: u64 = 32;
    pub(crate) const P_INPUT_ASSEMBLY_STATE: u64 = 40;
    pub(crate) const P_TESSELLATION_STATE: u64 = 48;
    pub(crate) const P_VIEWPORT_STATE: u64 = 56;
    pub(crate) const P_RASTERIZATION_STATE: u64 = 64;
    pub(crate) const P_MULTISAMPLE_STATE: u64 = 72;
    pub(crate) const P_DEPTH_STENCIL_STATE: u64 = 80;
    pub(crate) const P_COLOR_BLEND_STATE: u64 = 88;
    pub(crate) const P_DYNAMIC_STATE: u64 = 96;
    pub(crate) const LAYOUT: u64 = 104;
    pub(crate) const RENDER_PASS: u64 = 112;
    pub(crate) const SUBPASS: u64 = 120;
    pub(crate) const BASE_PIPELINE_HANDLE: u64 = 128;
    pub(crate) const BASE_PIPELINE_INDEX: u64 = 136;
    pub(crate) const SIZE: u64 = 144;
}

/// `VkComputePipelineCreateInfo`.
pub(crate) mod compute_pipeline_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const STAGE: u64 = 24;
    pub(crate) const LAYOUT: u64 = 72;
    pub(crate) const BASE_PIPELINE_HANDLE: u64 = 80;
    pub(crate) const BASE_PIPELINE_INDEX: u64 = 88;
    pub(crate) const SIZE: u64 = 96;
}
