//! Field offsets and sizes of the Vulkan structs for the structs that the host fills in: device properties, features, memory, formats, surfaces, extensions (64-bit layout).

/// `VkBaseOutStructure`.
pub(crate) mod base_out_structure {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const SIZE: u64 = 16;
}

/// `VkPhysicalDeviceLimits`.
pub(crate) mod physical_device_limits {
    pub(crate) const MAX_IMAGE_DIMENSION1D: u64 = 0;
    pub(crate) const MAX_IMAGE_DIMENSION2D: u64 = 4;
    pub(crate) const MAX_IMAGE_DIMENSION3D: u64 = 8;
    pub(crate) const MAX_IMAGE_DIMENSION_CUBE: u64 = 12;
    pub(crate) const MAX_IMAGE_ARRAY_LAYERS: u64 = 16;
    pub(crate) const MAX_TEXEL_BUFFER_ELEMENTS: u64 = 20;
    pub(crate) const MAX_UNIFORM_BUFFER_RANGE: u64 = 24;
    pub(crate) const MAX_STORAGE_BUFFER_RANGE: u64 = 28;
    pub(crate) const MAX_PUSH_CONSTANTS_SIZE: u64 = 32;
    pub(crate) const MAX_MEMORY_ALLOCATION_COUNT: u64 = 36;
    pub(crate) const MAX_SAMPLER_ALLOCATION_COUNT: u64 = 40;
    pub(crate) const BUFFER_IMAGE_GRANULARITY: u64 = 48;
    pub(crate) const SPARSE_ADDRESS_SPACE_SIZE: u64 = 56;
    pub(crate) const MAX_BOUND_DESCRIPTOR_SETS: u64 = 64;
    pub(crate) const MAX_PER_STAGE_DESCRIPTOR_SAMPLERS: u64 = 68;
    pub(crate) const MAX_PER_STAGE_DESCRIPTOR_UNIFORM_BUFFERS: u64 = 72;
    pub(crate) const MAX_PER_STAGE_DESCRIPTOR_STORAGE_BUFFERS: u64 = 76;
    pub(crate) const MAX_PER_STAGE_DESCRIPTOR_SAMPLED_IMAGES: u64 = 80;
    pub(crate) const MAX_PER_STAGE_DESCRIPTOR_STORAGE_IMAGES: u64 = 84;
    pub(crate) const MAX_PER_STAGE_DESCRIPTOR_INPUT_ATTACHMENTS: u64 = 88;
    pub(crate) const MAX_PER_STAGE_RESOURCES: u64 = 92;
    pub(crate) const MAX_DESCRIPTOR_SET_SAMPLERS: u64 = 96;
    pub(crate) const MAX_DESCRIPTOR_SET_UNIFORM_BUFFERS: u64 = 100;
    pub(crate) const MAX_DESCRIPTOR_SET_UNIFORM_BUFFERS_DYNAMIC: u64 = 104;
    pub(crate) const MAX_DESCRIPTOR_SET_STORAGE_BUFFERS: u64 = 108;
    pub(crate) const MAX_DESCRIPTOR_SET_STORAGE_BUFFERS_DYNAMIC: u64 = 112;
    pub(crate) const MAX_DESCRIPTOR_SET_SAMPLED_IMAGES: u64 = 116;
    pub(crate) const MAX_DESCRIPTOR_SET_STORAGE_IMAGES: u64 = 120;
    pub(crate) const MAX_DESCRIPTOR_SET_INPUT_ATTACHMENTS: u64 = 124;
    pub(crate) const MAX_VERTEX_INPUT_ATTRIBUTES: u64 = 128;
    pub(crate) const MAX_VERTEX_INPUT_BINDINGS: u64 = 132;
    pub(crate) const MAX_VERTEX_INPUT_ATTRIBUTE_OFFSET: u64 = 136;
    pub(crate) const MAX_VERTEX_INPUT_BINDING_STRIDE: u64 = 140;
    pub(crate) const MAX_VERTEX_OUTPUT_COMPONENTS: u64 = 144;
    pub(crate) const MAX_TESSELLATION_GENERATION_LEVEL: u64 = 148;
    pub(crate) const MAX_TESSELLATION_PATCH_SIZE: u64 = 152;
    pub(crate) const MAX_TESSELLATION_CONTROL_PER_VERTEX_INPUT_COMPONENTS: u64 = 156;
    pub(crate) const MAX_TESSELLATION_CONTROL_PER_VERTEX_OUTPUT_COMPONENTS: u64 = 160;
    pub(crate) const MAX_TESSELLATION_CONTROL_PER_PATCH_OUTPUT_COMPONENTS: u64 = 164;
    pub(crate) const MAX_TESSELLATION_CONTROL_TOTAL_OUTPUT_COMPONENTS: u64 = 168;
    pub(crate) const MAX_TESSELLATION_EVALUATION_INPUT_COMPONENTS: u64 = 172;
    pub(crate) const MAX_TESSELLATION_EVALUATION_OUTPUT_COMPONENTS: u64 = 176;
    pub(crate) const MAX_GEOMETRY_SHADER_INVOCATIONS: u64 = 180;
    pub(crate) const MAX_GEOMETRY_INPUT_COMPONENTS: u64 = 184;
    pub(crate) const MAX_GEOMETRY_OUTPUT_COMPONENTS: u64 = 188;
    pub(crate) const MAX_GEOMETRY_OUTPUT_VERTICES: u64 = 192;
    pub(crate) const MAX_GEOMETRY_TOTAL_OUTPUT_COMPONENTS: u64 = 196;
    pub(crate) const MAX_FRAGMENT_INPUT_COMPONENTS: u64 = 200;
    pub(crate) const MAX_FRAGMENT_OUTPUT_ATTACHMENTS: u64 = 204;
    pub(crate) const MAX_FRAGMENT_DUAL_SRC_ATTACHMENTS: u64 = 208;
    pub(crate) const MAX_FRAGMENT_COMBINED_OUTPUT_RESOURCES: u64 = 212;
    pub(crate) const MAX_COMPUTE_SHARED_MEMORY_SIZE: u64 = 216;
    pub(crate) const MAX_COMPUTE_WORK_GROUP_COUNT: u64 = 220;
    pub(crate) const MAX_COMPUTE_WORK_GROUP_INVOCATIONS: u64 = 232;
    pub(crate) const MAX_COMPUTE_WORK_GROUP_SIZE: u64 = 236;
    pub(crate) const SUB_PIXEL_PRECISION_BITS: u64 = 248;
    pub(crate) const SUB_TEXEL_PRECISION_BITS: u64 = 252;
    pub(crate) const MIPMAP_PRECISION_BITS: u64 = 256;
    pub(crate) const MAX_DRAW_INDEXED_INDEX_VALUE: u64 = 260;
    pub(crate) const MAX_DRAW_INDIRECT_COUNT: u64 = 264;
    pub(crate) const MAX_SAMPLER_LOD_BIAS: u64 = 268;
    pub(crate) const MAX_SAMPLER_ANISOTROPY: u64 = 272;
    pub(crate) const MAX_VIEWPORTS: u64 = 276;
    pub(crate) const MAX_VIEWPORT_DIMENSIONS: u64 = 280;
    pub(crate) const VIEWPORT_BOUNDS_RANGE: u64 = 288;
    pub(crate) const VIEWPORT_SUB_PIXEL_BITS: u64 = 296;
    pub(crate) const MIN_MEMORY_MAP_ALIGNMENT: u64 = 304;
    pub(crate) const MIN_TEXEL_BUFFER_OFFSET_ALIGNMENT: u64 = 312;
    pub(crate) const MIN_UNIFORM_BUFFER_OFFSET_ALIGNMENT: u64 = 320;
    pub(crate) const MIN_STORAGE_BUFFER_OFFSET_ALIGNMENT: u64 = 328;
    pub(crate) const MIN_TEXEL_OFFSET: u64 = 336;
    pub(crate) const MAX_TEXEL_OFFSET: u64 = 340;
    pub(crate) const MIN_TEXEL_GATHER_OFFSET: u64 = 344;
    pub(crate) const MAX_TEXEL_GATHER_OFFSET: u64 = 348;
    pub(crate) const MIN_INTERPOLATION_OFFSET: u64 = 352;
    pub(crate) const MAX_INTERPOLATION_OFFSET: u64 = 356;
    pub(crate) const SUB_PIXEL_INTERPOLATION_OFFSET_BITS: u64 = 360;
    pub(crate) const MAX_FRAMEBUFFER_WIDTH: u64 = 364;
    pub(crate) const MAX_FRAMEBUFFER_HEIGHT: u64 = 368;
    pub(crate) const MAX_FRAMEBUFFER_LAYERS: u64 = 372;
    pub(crate) const FRAMEBUFFER_COLOR_SAMPLE_COUNTS: u64 = 376;
    pub(crate) const FRAMEBUFFER_DEPTH_SAMPLE_COUNTS: u64 = 380;
    pub(crate) const FRAMEBUFFER_STENCIL_SAMPLE_COUNTS: u64 = 384;
    pub(crate) const FRAMEBUFFER_NO_ATTACHMENTS_SAMPLE_COUNTS: u64 = 388;
    pub(crate) const MAX_COLOR_ATTACHMENTS: u64 = 392;
    pub(crate) const SAMPLED_IMAGE_COLOR_SAMPLE_COUNTS: u64 = 396;
    pub(crate) const SAMPLED_IMAGE_INTEGER_SAMPLE_COUNTS: u64 = 400;
    pub(crate) const SAMPLED_IMAGE_DEPTH_SAMPLE_COUNTS: u64 = 404;
    pub(crate) const SAMPLED_IMAGE_STENCIL_SAMPLE_COUNTS: u64 = 408;
    pub(crate) const STORAGE_IMAGE_SAMPLE_COUNTS: u64 = 412;
    pub(crate) const MAX_SAMPLE_MASK_WORDS: u64 = 416;
    pub(crate) const TIMESTAMP_COMPUTE_AND_GRAPHICS: u64 = 420;
    pub(crate) const TIMESTAMP_PERIOD: u64 = 424;
    pub(crate) const MAX_CLIP_DISTANCES: u64 = 428;
    pub(crate) const MAX_CULL_DISTANCES: u64 = 432;
    pub(crate) const MAX_COMBINED_CLIP_AND_CULL_DISTANCES: u64 = 436;
    pub(crate) const DISCRETE_QUEUE_PRIORITIES: u64 = 440;
    pub(crate) const POINT_SIZE_RANGE: u64 = 444;
    pub(crate) const LINE_WIDTH_RANGE: u64 = 452;
    pub(crate) const POINT_SIZE_GRANULARITY: u64 = 460;
    pub(crate) const LINE_WIDTH_GRANULARITY: u64 = 464;
    pub(crate) const STRICT_LINES: u64 = 468;
    pub(crate) const STANDARD_SAMPLE_LOCATIONS: u64 = 472;
    pub(crate) const OPTIMAL_BUFFER_COPY_OFFSET_ALIGNMENT: u64 = 480;
    pub(crate) const OPTIMAL_BUFFER_COPY_ROW_PITCH_ALIGNMENT: u64 = 488;
    pub(crate) const NON_COHERENT_ATOM_SIZE: u64 = 496;
    pub(crate) const SIZE: u64 = 504;
}

/// `VkPhysicalDeviceSparseProperties`.
pub(crate) mod physical_device_sparse_properties {
    pub(crate) const RESIDENCY_STANDARD2D_BLOCK_SHAPE: u64 = 0;
    pub(crate) const RESIDENCY_STANDARD2D_MULTISAMPLE_BLOCK_SHAPE: u64 = 4;
    pub(crate) const RESIDENCY_STANDARD3D_BLOCK_SHAPE: u64 = 8;
    pub(crate) const RESIDENCY_ALIGNED_MIP_SIZE: u64 = 12;
    pub(crate) const RESIDENCY_NON_RESIDENT_STRICT: u64 = 16;
    pub(crate) const SIZE: u64 = 20;
}

/// `VkPhysicalDeviceProperties`.
pub(crate) mod physical_device_properties {
    pub(crate) const API_VERSION: u64 = 0;
    pub(crate) const DRIVER_VERSION: u64 = 4;
    pub(crate) const VENDOR_ID: u64 = 8;
    pub(crate) const DEVICE_ID: u64 = 12;
    pub(crate) const DEVICE_TYPE: u64 = 16;
    pub(crate) const DEVICE_NAME: u64 = 20;
    pub(crate) const PIPELINE_CACHE_UUID: u64 = 276;
    pub(crate) const LIMITS: u64 = 296;
    pub(crate) const SPARSE_PROPERTIES: u64 = 800;
    pub(crate) const SIZE: u64 = 824;
}

/// `VkPhysicalDeviceProperties2`.
pub(crate) mod physical_device_properties2 {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const PROPERTIES: u64 = 16;
    pub(crate) const SIZE: u64 = 840;
}

/// `VkPhysicalDeviceFeatures2`.
pub(crate) mod physical_device_features2 {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const FEATURES: u64 = 16;
    pub(crate) const SIZE: u64 = 240;
}

/// `VkMemoryType`.
pub(crate) mod memory_type {
    pub(crate) const PROPERTY_FLAGS: u64 = 0;
    pub(crate) const HEAP_INDEX: u64 = 4;
    pub(crate) const SIZE: u64 = 8;
}

/// `VkMemoryHeap`.
pub(crate) mod memory_heap {
    pub(crate) const SIZE_: u64 = 0;
    pub(crate) const FLAGS: u64 = 8;
    pub(crate) const SIZE: u64 = 16;
}

/// `VkPhysicalDeviceMemoryProperties`.
pub(crate) mod physical_device_memory_properties {
    pub(crate) const MEMORY_TYPE_COUNT: u64 = 0;
    pub(crate) const MEMORY_TYPES: u64 = 4;
    pub(crate) const MEMORY_HEAP_COUNT: u64 = 260;
    pub(crate) const MEMORY_HEAPS: u64 = 264;
    pub(crate) const SIZE: u64 = 520;
}

/// `VkPhysicalDeviceMemoryProperties2`.
pub(crate) mod physical_device_memory_properties2 {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const MEMORY_PROPERTIES: u64 = 16;
    pub(crate) const SIZE: u64 = 536;
}

/// `VkQueueFamilyProperties`.
pub(crate) mod queue_family_properties {
    pub(crate) const QUEUE_FLAGS: u64 = 0;
    pub(crate) const QUEUE_COUNT: u64 = 4;
    pub(crate) const TIMESTAMP_VALID_BITS: u64 = 8;
    pub(crate) const MIN_IMAGE_TRANSFER_GRANULARITY: u64 = 12;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkFormatProperties`.
pub(crate) mod format_properties {
    pub(crate) const LINEAR_TILING_FEATURES: u64 = 0;
    pub(crate) const OPTIMAL_TILING_FEATURES: u64 = 4;
    pub(crate) const BUFFER_FEATURES: u64 = 8;
    pub(crate) const SIZE: u64 = 12;
}

/// `VkImageFormatProperties`.
pub(crate) mod image_format_properties {
    pub(crate) const MAX_EXTENT: u64 = 0;
    pub(crate) const MAX_MIP_LEVELS: u64 = 12;
    pub(crate) const MAX_ARRAY_LAYERS: u64 = 16;
    pub(crate) const SAMPLE_COUNTS: u64 = 20;
    pub(crate) const MAX_RESOURCE_SIZE: u64 = 24;
    pub(crate) const SIZE: u64 = 32;
}

/// `VkSurfaceCapabilitiesKHR`.
pub(crate) mod surface_capabilities_khr {
    pub(crate) const MIN_IMAGE_COUNT: u64 = 0;
    pub(crate) const MAX_IMAGE_COUNT: u64 = 4;
    pub(crate) const CURRENT_EXTENT: u64 = 8;
    pub(crate) const MIN_IMAGE_EXTENT: u64 = 16;
    pub(crate) const MAX_IMAGE_EXTENT: u64 = 24;
    pub(crate) const MAX_IMAGE_ARRAY_LAYERS: u64 = 32;
    pub(crate) const SUPPORTED_TRANSFORMS: u64 = 36;
    pub(crate) const CURRENT_TRANSFORM: u64 = 40;
    pub(crate) const SUPPORTED_COMPOSITE_ALPHA: u64 = 44;
    pub(crate) const SUPPORTED_USAGE_FLAGS: u64 = 48;
    pub(crate) const SIZE: u64 = 52;
}

/// `VkSurfaceFormatKHR`.
pub(crate) mod surface_format_khr {
    pub(crate) const FORMAT: u64 = 0;
    pub(crate) const COLOR_SPACE: u64 = 4;
    pub(crate) const SIZE: u64 = 8;
}

/// `VkExtensionProperties`.
pub(crate) mod extension_properties {
    pub(crate) const EXTENSION_NAME: u64 = 0;
    pub(crate) const SPEC_VERSION: u64 = 256;
    pub(crate) const SIZE: u64 = 260;
}

/// `VkSubresourceLayout`.
pub(crate) mod subresource_layout {
    pub(crate) const OFFSET: u64 = 0;
    pub(crate) const SIZE_: u64 = 8;
    pub(crate) const ROW_PITCH: u64 = 16;
    pub(crate) const ARRAY_PITCH: u64 = 24;
    pub(crate) const DEPTH_PITCH: u64 = 32;
    pub(crate) const SIZE: u64 = 40;
}

/// `VkPhysicalDeviceGroupProperties`.
pub(crate) mod physical_device_group_properties {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const PHYSICAL_DEVICE_COUNT: u64 = 16;
    pub(crate) const PHYSICAL_DEVICES: u64 = 24;
    pub(crate) const SUBSET_ALLOCATION: u64 = 280;
    pub(crate) const SIZE: u64 = 288;
}

/// `VkMemoryRequirements`.
pub(crate) mod memory_requirements {
    pub(crate) const SIZE_: u64 = 0;
    pub(crate) const ALIGNMENT: u64 = 8;
    pub(crate) const MEMORY_TYPE_BITS: u64 = 16;
    pub(crate) const SIZE: u64 = 24;
}

/// `VkMemoryRequirements2`.
pub(crate) mod memory_requirements2 {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const MEMORY_REQUIREMENTS: u64 = 16;
    pub(crate) const SIZE: u64 = 40;
}

/// `VkMemoryDedicatedRequirements`.
pub(crate) mod memory_dedicated_requirements {
    pub(crate) const S_TYPE: u64 = 0;
    pub(crate) const P_NEXT: u64 = 8;
    pub(crate) const PREFERS_DEDICATED_ALLOCATION: u64 = 16;
    pub(crate) const REQUIRES_DEDICATED_ALLOCATION: u64 = 20;
    pub(crate) const SIZE: u64 = 24;
}

/// The fields of `VkPhysicalDeviceLimits` by name, with their offsets.
pub(crate) const LIMIT_FIELDS: &[(&str, u64)] = &[
    ("maxImageDimension1D", 0),
    ("maxImageDimension2D", 4),
    ("maxImageDimension3D", 8),
    ("maxImageDimensionCube", 12),
    ("maxImageArrayLayers", 16),
    ("maxTexelBufferElements", 20),
    ("maxUniformBufferRange", 24),
    ("maxStorageBufferRange", 28),
    ("maxPushConstantsSize", 32),
    ("maxMemoryAllocationCount", 36),
    ("maxSamplerAllocationCount", 40),
    ("bufferImageGranularity", 48),
    ("sparseAddressSpaceSize", 56),
    ("maxBoundDescriptorSets", 64),
    ("maxPerStageDescriptorSamplers", 68),
    ("maxPerStageDescriptorUniformBuffers", 72),
    ("maxPerStageDescriptorStorageBuffers", 76),
    ("maxPerStageDescriptorSampledImages", 80),
    ("maxPerStageDescriptorStorageImages", 84),
    ("maxPerStageDescriptorInputAttachments", 88),
    ("maxPerStageResources", 92),
    ("maxDescriptorSetSamplers", 96),
    ("maxDescriptorSetUniformBuffers", 100),
    ("maxDescriptorSetUniformBuffersDynamic", 104),
    ("maxDescriptorSetStorageBuffers", 108),
    ("maxDescriptorSetStorageBuffersDynamic", 112),
    ("maxDescriptorSetSampledImages", 116),
    ("maxDescriptorSetStorageImages", 120),
    ("maxDescriptorSetInputAttachments", 124),
    ("maxVertexInputAttributes", 128),
    ("maxVertexInputBindings", 132),
    ("maxVertexInputAttributeOffset", 136),
    ("maxVertexInputBindingStride", 140),
    ("maxVertexOutputComponents", 144),
    ("maxTessellationGenerationLevel", 148),
    ("maxTessellationPatchSize", 152),
    ("maxTessellationControlPerVertexInputComponents", 156),
    ("maxTessellationControlPerVertexOutputComponents", 160),
    ("maxTessellationControlPerPatchOutputComponents", 164),
    ("maxTessellationControlTotalOutputComponents", 168),
    ("maxTessellationEvaluationInputComponents", 172),
    ("maxTessellationEvaluationOutputComponents", 176),
    ("maxGeometryShaderInvocations", 180),
    ("maxGeometryInputComponents", 184),
    ("maxGeometryOutputComponents", 188),
    ("maxGeometryOutputVertices", 192),
    ("maxGeometryTotalOutputComponents", 196),
    ("maxFragmentInputComponents", 200),
    ("maxFragmentOutputAttachments", 204),
    ("maxFragmentDualSrcAttachments", 208),
    ("maxFragmentCombinedOutputResources", 212),
    ("maxComputeSharedMemorySize", 216),
    ("maxComputeWorkGroupCount", 220),
    ("maxComputeWorkGroupInvocations", 232),
    ("maxComputeWorkGroupSize", 236),
    ("subPixelPrecisionBits", 248),
    ("subTexelPrecisionBits", 252),
    ("mipmapPrecisionBits", 256),
    ("maxDrawIndexedIndexValue", 260),
    ("maxDrawIndirectCount", 264),
    ("maxSamplerLodBias", 268),
    ("maxSamplerAnisotropy", 272),
    ("maxViewports", 276),
    ("maxViewportDimensions", 280),
    ("viewportBoundsRange", 288),
    ("viewportSubPixelBits", 296),
    ("minMemoryMapAlignment", 304),
    ("minTexelBufferOffsetAlignment", 312),
    ("minUniformBufferOffsetAlignment", 320),
    ("minStorageBufferOffsetAlignment", 328),
    ("minTexelOffset", 336),
    ("maxTexelOffset", 340),
    ("minTexelGatherOffset", 344),
    ("maxTexelGatherOffset", 348),
    ("minInterpolationOffset", 352),
    ("maxInterpolationOffset", 356),
    ("subPixelInterpolationOffsetBits", 360),
    ("maxFramebufferWidth", 364),
    ("maxFramebufferHeight", 368),
    ("maxFramebufferLayers", 372),
    ("framebufferColorSampleCounts", 376),
    ("framebufferDepthSampleCounts", 380),
    ("framebufferStencilSampleCounts", 384),
    ("framebufferNoAttachmentsSampleCounts", 388),
    ("maxColorAttachments", 392),
    ("sampledImageColorSampleCounts", 396),
    ("sampledImageIntegerSampleCounts", 400),
    ("sampledImageDepthSampleCounts", 404),
    ("sampledImageStencilSampleCounts", 408),
    ("storageImageSampleCounts", 412),
    ("maxSampleMaskWords", 416),
    ("timestampComputeAndGraphics", 420),
    ("timestampPeriod", 424),
    ("maxClipDistances", 428),
    ("maxCullDistances", 432),
    ("maxCombinedClipAndCullDistances", 436),
    ("discreteQueuePriorities", 440),
    ("pointSizeRange", 444),
    ("lineWidthRange", 452),
    ("pointSizeGranularity", 460),
    ("lineWidthGranularity", 464),
    ("strictLines", 468),
    ("standardSampleLocations", 472),
    ("optimalBufferCopyOffsetAlignment", 480),
    ("optimalBufferCopyRowPitchAlignment", 488),
    ("nonCoherentAtomSize", 496),
];
