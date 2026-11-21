//! Field offsets and sizes of the Vulkan structs for instances, devices, memory, resources, descriptors and pipeline layouts (64-bit layout).

/// `VkBaseInStructure`.
pub(crate) mod base_in_structure {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const SIZE: u64 = 16;
}

/// `VkApplicationInfo`.
pub(crate) mod application_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const P_APPLICATION_NAME: u64 = 16;
    pub(crate) const APPLICATION_VERSION: u64 = 24;
    pub(crate) const P_ENGINE_NAME: u64 = 32;
    pub(crate) const ENGINE_VERSION: u64 = 40;
    pub(crate) const API_VERSION: u64 = 44;
    pub(crate) const SIZE: u64 = 48;
}

/// `VkInstanceCreateInfo`.
pub(crate) mod instance_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const P_APPLICATION_INFO: u64 = 24;
    pub(crate) const ENABLED_LAYER_COUNT: u64 = 32;
    pub(crate) const PP_ENABLED_LAYER_NAMES: u64 = 40;
    pub(crate) const ENABLED_EXTENSION_COUNT: u64 = 48;
    pub(crate) const PP_ENABLED_EXTENSION_NAMES: u64 = 56;
    pub(crate) const SIZE: u64 = 64;
}

/// `VkDeviceQueueCreateInfo`.
pub(crate) mod device_queue_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const QUEUE_FAMILY_INDEX: u64 = 20;
    pub(crate) const QUEUE_COUNT: u64 = 24;
    pub(crate) const P_QUEUE_PRIORITIES: u64 = 32;
    pub(crate) const SIZE: u64 = 40;
}

/// `VkDeviceCreateInfo`.
pub(crate) mod device_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const QUEUE_CREATE_INFO_COUNT: u64 = 20;
    pub(crate) const P_QUEUE_CREATE_INFOS: u64 = 24;
    pub(crate) const ENABLED_LAYER_COUNT: u64 = 32;
    pub(crate) const PP_ENABLED_LAYER_NAMES: u64 = 40;
    pub(crate) const ENABLED_EXTENSION_COUNT: u64 = 48;
    pub(crate) const PP_ENABLED_EXTENSION_NAMES: u64 = 56;
    pub(crate) const P_ENABLED_FEATURES: u64 = 64;
    pub(crate) const SIZE: u64 = 72;
}

/// `VkMemoryAllocateInfo`.
pub(crate) mod memory_allocate_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const ALLOCATION_SIZE: u64 = 16;
    pub(crate) const MEMORY_TYPE_INDEX: u64 = 24;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkBufferCreateInfo`.
pub(crate) mod buffer_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const SIZE_: u64 = 24;
    pub(crate) const USAGE: u64 = 32;
    pub(crate) const SHARING_MODE: u64 = 36;
    pub(crate) const QUEUE_FAMILY_INDEX_COUNT: u64 = 40;
    pub(crate) const P_QUEUE_FAMILY_INDICES: u64 = 48;
    pub(crate) const SIZE: u64 = 56;
}

/// `VkBufferViewCreateInfo`.
pub(crate) mod buffer_view_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const BUFFER: u64 = 24;
    pub(crate) const FORMAT: u64 = 32;
    pub(crate) const OFFSET: u64 = 40;
    pub(crate) const RANGE: u64 = 48;
    pub(crate) const SIZE: u64 = 56;
}

/// `VkExtent3D`.
pub(crate) mod extent3d {
    pub(crate) const WIDTH: u64 = 0;
    pub(crate) const HEIGHT: u64 = 4;
    pub(crate) const DEPTH: u64 = 8;
    pub(crate) const SIZE: u64 = 12;
}

/// `VkOffset3D`.
pub(crate) mod offset3d {
    pub(crate) const X: u64 = 0;
    pub(crate) const Y: u64 = 4;
    pub(crate) const Z: u64 = 8;
    pub(crate) const SIZE: u64 = 12;
}

/// `VkExtent2D`.
pub(crate) mod extent2d {
    pub(crate) const WIDTH: u64 = 0;
    pub(crate) const HEIGHT: u64 = 4;
    pub(crate) const SIZE: u64 = 8;
}

/// `VkOffset2D`.
pub(crate) mod offset2d {
    pub(crate) const X: u64 = 0;
    pub(crate) const Y: u64 = 4;
    pub(crate) const SIZE: u64 = 8;
}

/// `VkImageCreateInfo`.
pub(crate) mod image_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const IMAGE_TYPE: u64 = 20;
    pub(crate) const FORMAT: u64 = 24;
    pub(crate) const EXTENT: u64 = 28;
    pub(crate) const MIP_LEVELS: u64 = 40;
    pub(crate) const ARRAY_LAYERS: u64 = 44;
    pub(crate) const SAMPLES: u64 = 48;
    pub(crate) const TILING: u64 = 52;
    pub(crate) const USAGE: u64 = 56;
    pub(crate) const SHARING_MODE: u64 = 60;
    pub(crate) const QUEUE_FAMILY_INDEX_COUNT: u64 = 64;
    pub(crate) const P_QUEUE_FAMILY_INDICES: u64 = 72;
    pub(crate) const INITIAL_LAYOUT: u64 = 80;
    pub(crate) const SIZE: u64 = 88;
}

/// `VkComponentMapping`.
pub(crate) mod component_mapping {
    pub(crate) const R: u64 = 0;
    pub(crate) const G: u64 = 4;
    pub(crate) const B: u64 = 8;
    pub(crate) const A: u64 = 12;
    pub(crate) const SIZE: u64 = 16;
}

/// `VkImageSubresourceRange`.
pub(crate) mod image_subresource_range {
    pub(crate) const ASPECT_MASK: u64 = 0;
    pub(crate) const BASE_MIP_LEVEL: u64 = 4;
    pub(crate) const LEVEL_COUNT: u64 = 8;
    pub(crate) const BASE_ARRAY_LAYER: u64 = 12;
    pub(crate) const LAYER_COUNT: u64 = 16;
    pub(crate) const SIZE: u64 = 20;
}

/// `VkImageViewCreateInfo`.
pub(crate) mod image_view_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const IMAGE: u64 = 24;
    pub(crate) const VIEW_TYPE: u64 = 32;
    pub(crate) const FORMAT: u64 = 36;
    pub(crate) const COMPONENTS: u64 = 40;
    pub(crate) const SUBRESOURCE_RANGE: u64 = 56;
    pub(crate) const SIZE: u64 = 80;
}

/// `VkSamplerCreateInfo`.
pub(crate) mod sampler_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const MAG_FILTER: u64 = 20;
    pub(crate) const MIN_FILTER: u64 = 24;
    pub(crate) const MIPMAP_MODE: u64 = 28;
    pub(crate) const ADDRESS_MODE_U: u64 = 32;
    pub(crate) const ADDRESS_MODE_V: u64 = 36;
    pub(crate) const ADDRESS_MODE_W: u64 = 40;
    pub(crate) const MIP_LOD_BIAS: u64 = 44;
    pub(crate) const ANISOTROPY_ENABLE: u64 = 48;
    pub(crate) const MAX_ANISOTROPY: u64 = 52;
    pub(crate) const COMPARE_ENABLE: u64 = 56;
    pub(crate) const COMPARE_OP: u64 = 60;
    pub(crate) const MIN_LOD: u64 = 64;
    pub(crate) const MAX_LOD: u64 = 68;
    pub(crate) const BORDER_COLOR: u64 = 72;
    pub(crate) const UNNORMALIZED_COORDINATES: u64 = 76;
    pub(crate) const SIZE: u64 = 80;
}

/// `VkBindBufferMemoryInfo`.
pub(crate) mod bind_buffer_memory_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const BUFFER: u64 = 16;
    pub(crate) const MEMORY: u64 = 24;
    pub(crate) const MEMORY_OFFSET: u64 = 32;
    pub(crate) const SIZE: u64 = 40;
}

/// `VkBindImageMemoryInfo`.
pub(crate) mod bind_image_memory_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const IMAGE: u64 = 16;
    pub(crate) const MEMORY: u64 = 24;
    pub(crate) const MEMORY_OFFSET: u64 = 32;
    pub(crate) const SIZE: u64 = 40;
}

/// `VkBufferMemoryRequirementsInfo2`.
pub(crate) mod buffer_memory_requirements_info2 {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const BUFFER: u64 = 16;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkImageMemoryRequirementsInfo2`.
pub(crate) mod image_memory_requirements_info2 {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const IMAGE: u64 = 16;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkDescriptorSetLayoutBinding`.
pub(crate) mod descriptor_set_layout_binding {
    pub(crate) const BINDING: u64 = 0;
    pub(crate) const DESCRIPTOR_TYPE: u64 = 4;
    pub(crate) const DESCRIPTOR_COUNT: u64 = 8;
    pub(crate) const STAGE_FLAGS: u64 = 12;
    pub(crate) const P_IMMUTABLE_SAMPLERS: u64 = 16;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkDescriptorSetLayoutCreateInfo`.
pub(crate) mod descriptor_set_layout_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const BINDING_COUNT: u64 = 20;
    pub(crate) const P_BINDINGS: u64 = 24;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkDescriptorPoolCreateInfo`.
pub(crate) mod descriptor_pool_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const MAX_SETS: u64 = 20;
    pub(crate) const POOL_SIZE_COUNT: u64 = 24;
    pub(crate) const P_POOL_SIZES: u64 = 32;
    pub(crate) const SIZE: u64 = 40;
}

/// `VkDescriptorSetAllocateInfo`.
pub(crate) mod descriptor_set_allocate_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const DESCRIPTOR_POOL: u64 = 16;
    pub(crate) const DESCRIPTOR_SET_COUNT: u64 = 24;
    pub(crate) const P_SET_LAYOUTS: u64 = 32;
    pub(crate) const SIZE: u64 = 40;
}

/// `VkDescriptorImageInfo`.
pub(crate) mod descriptor_image_info {
    pub(crate) const SAMPLER: u64 = 0;
    pub(crate) const IMAGE_VIEW: u64 = 8;
    pub(crate) const IMAGE_LAYOUT: u64 = 16;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkDescriptorBufferInfo`.
pub(crate) mod descriptor_buffer_info {
    pub(crate) const BUFFER: u64 = 0;
    pub(crate) const OFFSET: u64 = 8;
    pub(crate) const RANGE: u64 = 16;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkWriteDescriptorSet`.
pub(crate) mod write_descriptor_set {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const DST_SET: u64 = 16;
    pub(crate) const DST_BINDING: u64 = 24;
    pub(crate) const DST_ARRAY_ELEMENT: u64 = 28;
    pub(crate) const DESCRIPTOR_COUNT: u64 = 32;
    pub(crate) const DESCRIPTOR_TYPE: u64 = 36;
    pub(crate) const P_IMAGE_INFO: u64 = 40;
    pub(crate) const P_BUFFER_INFO: u64 = 48;
    pub(crate) const P_TEXEL_BUFFER_VIEW: u64 = 56;
    pub(crate) const SIZE: u64 = 64;
}

/// `VkCopyDescriptorSet`.
pub(crate) mod copy_descriptor_set {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const SRC_SET: u64 = 16;
    pub(crate) const SRC_BINDING: u64 = 24;
    pub(crate) const SRC_ARRAY_ELEMENT: u64 = 28;
    pub(crate) const DST_SET: u64 = 32;
    pub(crate) const DST_BINDING: u64 = 40;
    pub(crate) const DST_ARRAY_ELEMENT: u64 = 44;
    pub(crate) const DESCRIPTOR_COUNT: u64 = 48;
    pub(crate) const SIZE: u64 = 56;
}

/// `VkDescriptorUpdateTemplateEntry`.
pub(crate) mod descriptor_update_template_entry {
    pub(crate) const DST_BINDING: u64 = 0;
    pub(crate) const DST_ARRAY_ELEMENT: u64 = 4;
    pub(crate) const DESCRIPTOR_COUNT: u64 = 8;
    pub(crate) const DESCRIPTOR_TYPE: u64 = 12;
    pub(crate) const OFFSET: u64 = 16;
    pub(crate) const STRIDE: u64 = 24;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkDescriptorUpdateTemplateCreateInfo`.
pub(crate) mod descriptor_update_template_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const DESCRIPTOR_UPDATE_ENTRY_COUNT: u64 = 20;
    pub(crate) const P_DESCRIPTOR_UPDATE_ENTRIES: u64 = 24;
    pub(crate) const TEMPLATE_TYPE: u64 = 32;
    pub(crate) const DESCRIPTOR_SET_LAYOUT: u64 = 40;
    pub(crate) const PIPELINE_BIND_POINT: u64 = 48;
    pub(crate) const PIPELINE_LAYOUT: u64 = 56;
    pub(crate) const SET: u64 = 64;
    pub(crate) const SIZE: u64 = 72;
}

/// `VkPushConstantRange`.
pub(crate) mod push_constant_range {
    pub(crate) const STAGE_FLAGS: u64 = 0;
    pub(crate) const OFFSET: u64 = 4;
    pub(crate) const SIZE_: u64 = 8;
    pub(crate) const SIZE: u64 = 12;
}

/// `VkPipelineLayoutCreateInfo`.
pub(crate) mod pipeline_layout_create_info {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FLAGS: u64 = 16;
    pub(crate) const SET_LAYOUT_COUNT: u64 = 20;
    pub(crate) const P_SET_LAYOUTS: u64 = 24;
    pub(crate) const PUSH_CONSTANT_RANGE_COUNT: u64 = 32;
    pub(crate) const P_PUSH_CONSTANT_RANGES: u64 = 40;
    pub(crate) const SIZE: u64 = 48;
}
