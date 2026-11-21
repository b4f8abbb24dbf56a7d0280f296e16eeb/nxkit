//! The one device that the game sees: a Tegra X1 under NVIDIA's Switch driver, Vulkan 1.1, one queue family, three
//! memory types on one 3 GB heap, except where Metal is stricter (the reference layer's device pose).
//!
//! Everything here is plain data. [`crate::write`] puts it into the structs of the game. What depends on the Metal
//! device (the viewport count, the read-write texture tier, the screen) comes from [`DeviceCaps`].

use crate::consts::{
    format_feature as ff, image_flags, image_type, image_usage, memory_property as mp, tiling,
};
use crate::format::{self, Class};
use crate::result::{VkError, VkResult};

/// What the backend's GPU decides about the device that the game sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceCaps {
    /// Viewports per pipeline: 16 on an Apple GPU of family 5 or newer, 1 otherwise.
    pub max_viewports: u32,
    /// Metal's read-write texture tier: 0 none, 1 (R32 formats), 2 (the common formats).
    pub rw_texture_tier: u8,
    /// The extent of the surface: 1920x1080 docked, 1280x720 handheld.
    pub surface_extent: (u32, u32),
}

impl Default for DeviceCaps {
    fn default() -> Self {
        Self {
            max_viewports: 16,
            rw_texture_tier: 2,
            surface_extent: (1280, 720),
        }
    }
}

/// The API version that the instance and the device report: Vulkan 1.1.
pub const API_VERSION: u32 = crate::consts::api_version(0, 1, 1, 0);

/// The extensions of the instance.
pub const INSTANCE_EXTENSIONS: &[&str] = &[
    "VK_KHR_surface",
    "VK_NN_vi_surface",
    "VK_KHR_get_physical_device_properties2",
    "VK_KHR_get_surface_capabilities2",
];

/// The extensions of the device. `VK_AMD_draw_indirect_count` is not among them: its commands are unsupported.
pub const DEVICE_EXTENSIONS: &[&str] = &[
    "VK_KHR_swapchain",
    "VK_KHR_descriptor_update_template",
    "VK_KHR_maintenance1",
    "VK_KHR_get_memory_requirements2",
    "VK_KHR_dedicated_allocation",
    "VK_KHR_bind_memory2",
];

/// The memory types: device local, host visible and coherent, host cached. All on heap 0.
pub const MEMORY_TYPES: [u32; 3] = [
    mp::DEVICE_LOCAL,
    mp::DEVICE_LOCAL | mp::HOST_VISIBLE | mp::HOST_COHERENT,
    mp::DEVICE_LOCAL | mp::HOST_VISIBLE | mp::HOST_COHERENT | mp::HOST_CACHED,
];

/// The size of the one heap: 3 GB.
pub const HEAP_SIZE: u64 = 3 << 30;

/// The bits of every memory type, for `memoryTypeBits`.
pub const ALL_MEMORY_TYPES: u32 = 0b111;

/// The alignment of a buffer in its memory: Metal's constant buffer offset alignment on the Mac.
pub const BUFFER_ALIGNMENT: u64 = 256;

/// The alignment of an image in its memory.
pub const IMAGE_ALIGNMENT: u64 = 4096;

/// The one queue family: graphics, compute and transfer, 16 queues, 64 timestamp bits. Every queue is the same serial
/// queue of the backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueFamily {
    /// `queueFlags`.
    pub flags: u32,
    /// `queueCount`.
    pub count: u32,
    /// `timestampValidBits`.
    pub timestamp_bits: u32,
    /// `minImageTransferGranularity`.
    pub granularity: [u32; 3],
}

/// The queue family of the device.
pub const QUEUE_FAMILY: QueueFamily = QueueFamily {
    flags: 0x7,
    count: 16,
    timestamp_bits: 64,
    granularity: [1, 1, 1],
};

/// The surface formats, all in `VK_COLOR_SPACE_SRGB_NONLINEAR_KHR` (0).
pub const SURFACE_FORMATS: [u32; 4] = [44, 37, 50, 43];

/// The present modes: `VK_PRESENT_MODE_FIFO_KHR` only.
pub const PRESENT_MODES: [u32; 1] = [2];

/// The identity of `VkPhysicalDeviceProperties`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// `driverVersion`: 1.0.0.
    pub driver_version: u32,
    /// `vendorID`: NVIDIA.
    pub vendor_id: u32,
    /// `deviceID`.
    pub device_id: u32,
    /// `deviceType`: integrated GPU.
    pub device_type: u32,
    /// `deviceName`.
    pub name: &'static str,
}

/// The identity of the device.
pub const IDENTITY: Identity = Identity {
    driver_version: crate::consts::api_version(0, 1, 0, 0),
    vendor_id: 0x10de,
    device_id: 0x92ba,
    device_type: 1,
    name: "NVIDIA Tegra X1 (nvgpu)",
};

/// The features that the device has, by name of `VkPhysicalDeviceFeatures` (`multiViewport` only with more than one
/// viewport, [`features`]).
pub const FEATURES: &[&str] = &[
    "robustBufferAccess",
    "fullDrawIndexUint32",
    "imageCubeArray",
    "independentBlend",
    "sampleRateShading",
    "dualSrcBlend",
    "drawIndirectFirstInstance",
    "depthClamp",
    "depthBiasClamp",
    "fillModeNonSolid",
    "largePoints",
    "samplerAnisotropy",
    "textureCompressionASTC_LDR",
    "textureCompressionBC",
    "occlusionQueryPrecise",
    "fragmentStoresAndAtomics",
    "shaderImageGatherExtended",
    "shaderClipDistance",
    "shaderResourceMinLod",
];

/// The 55 fields of `VkPhysicalDeviceFeatures` in their order.
pub const FEATURE_NAMES: [&str; 55] = [
    "robustBufferAccess",
    "fullDrawIndexUint32",
    "imageCubeArray",
    "independentBlend",
    "geometryShader",
    "tessellationShader",
    "sampleRateShading",
    "dualSrcBlend",
    "logicOp",
    "multiDrawIndirect",
    "drawIndirectFirstInstance",
    "depthClamp",
    "depthBiasClamp",
    "fillModeNonSolid",
    "depthBounds",
    "wideLines",
    "largePoints",
    "alphaToOne",
    "multiViewport",
    "samplerAnisotropy",
    "textureCompressionETC2",
    "textureCompressionASTC_LDR",
    "textureCompressionBC",
    "occlusionQueryPrecise",
    "pipelineStatisticsQuery",
    "vertexPipelineStoresAndAtomics",
    "fragmentStoresAndAtomics",
    "shaderTessellationAndGeometryPointSize",
    "shaderImageGatherExtended",
    "shaderStorageImageExtendedFormats",
    "shaderStorageImageMultisample",
    "shaderStorageImageReadWithoutFormat",
    "shaderStorageImageWriteWithoutFormat",
    "shaderUniformBufferArrayDynamicIndexing",
    "shaderSampledImageArrayDynamicIndexing",
    "shaderStorageBufferArrayDynamicIndexing",
    "shaderStorageImageArrayDynamicIndexing",
    "shaderClipDistance",
    "shaderCullDistance",
    "shaderFloat64",
    "shaderInt64",
    "shaderInt16",
    "shaderResourceResidency",
    "shaderResourceMinLod",
    "sparseBinding",
    "sparseResidencyBuffer",
    "sparseResidencyImage2D",
    "sparseResidencyImage3D",
    "sparseResidency2Samples",
    "sparseResidency4Samples",
    "sparseResidency8Samples",
    "sparseResidency16Samples",
    "sparseResidencyAliased",
    "variableMultisampleRate",
    "inheritedQueries",
];

/// The 55 `VkBool32` of `VkPhysicalDeviceFeatures`, in order.
pub fn features(caps: &DeviceCaps) -> [bool; 55] {
    FEATURE_NAMES.map(|n| FEATURES.contains(&n) || (n == "multiViewport" && caps.max_viewports > 1))
}

/// The limits that are not zero, by name of `VkPhysicalDeviceLimits` (the rest are zero: no tessellation, no geometry,
/// no sparse memory, no cull distances).
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// The `uint32_t`, `VkSampleCountFlags` and `VkBool32` limits.
    pub u32s: Vec<(&'static str, u32)>,
    /// The `uint32_t[3]` and `uint32_t[2]` limits.
    pub arrays: Vec<(&'static str, Vec<u32>)>,
    /// The `float` and `float[2]` limits.
    pub floats: Vec<(&'static str, Vec<f32>)>,
    /// The `int32_t` limits.
    pub i32s: Vec<(&'static str, i32)>,
    /// The `VkDeviceSize` and `size_t` limits.
    pub u64s: Vec<(&'static str, u64)>,
}

/// The limits of the device.
pub fn limits(caps: &DeviceCaps) -> Limits {
    let u32s = vec![
        ("maxImageDimension1D", 16384),
        ("maxImageDimension2D", 16384),
        ("maxImageDimension3D", 2048),
        ("maxImageDimensionCube", 16384),
        ("maxImageArrayLayers", 2048),
        ("maxTexelBufferElements", 1 << 27),
        ("maxUniformBufferRange", 65536),
        ("maxStorageBufferRange", 1 << 30),
        ("maxPushConstantsSize", 256),
        ("maxMemoryAllocationCount", 4096),
        ("maxSamplerAllocationCount", 4000),
        ("maxBoundDescriptorSets", 8),
        ("maxPerStageDescriptorSamplers", 16),
        ("maxPerStageDescriptorUniformBuffers", 15),
        ("maxPerStageDescriptorStorageBuffers", 16),
        ("maxPerStageDescriptorSampledImages", 31),
        ("maxPerStageDescriptorStorageImages", 8),
        ("maxPerStageDescriptorInputAttachments", 8),
        ("maxPerStageResources", 128),
        ("maxDescriptorSetSamplers", 4000),
        ("maxDescriptorSetUniformBuffers", 90),
        ("maxDescriptorSetUniformBuffersDynamic", 15),
        ("maxDescriptorSetStorageBuffers", 96),
        ("maxDescriptorSetStorageBuffersDynamic", 16),
        ("maxDescriptorSetSampledImages", 4096),
        ("maxDescriptorSetStorageImages", 4096),
        ("maxDescriptorSetInputAttachments", 8),
        ("maxVertexInputAttributes", 31),
        ("maxVertexInputBindings", 14),
        ("maxVertexInputAttributeOffset", 2047),
        ("maxVertexInputBindingStride", 2048),
        ("maxVertexOutputComponents", 128),
        ("maxFragmentInputComponents", 128),
        ("maxFragmentOutputAttachments", 8),
        ("maxFragmentDualSrcAttachments", 1),
        ("maxFragmentCombinedOutputResources", 16),
        ("maxComputeSharedMemorySize", 32768),
        ("maxComputeWorkGroupInvocations", 1024),
        ("subPixelPrecisionBits", 8),
        ("subTexelPrecisionBits", 8),
        ("mipmapPrecisionBits", 8),
        ("maxDrawIndexedIndexValue", u32::MAX),
        ("maxDrawIndirectCount", u32::MAX),
        ("maxViewports", caps.max_viewports),
        ("viewportSubPixelBits", 8),
        ("maxTexelOffset", 7),
        ("maxTexelGatherOffset", 31),
        ("subPixelInterpolationOffsetBits", 4),
        ("maxFramebufferWidth", 16384),
        ("maxFramebufferHeight", 16384),
        ("maxFramebufferLayers", 2048),
        ("framebufferColorSampleCounts", 0x7),
        ("framebufferDepthSampleCounts", 0x7),
        ("framebufferStencilSampleCounts", 0x7),
        ("framebufferNoAttachmentsSampleCounts", 0x7),
        ("maxColorAttachments", 8),
        ("sampledImageColorSampleCounts", 0x7),
        ("sampledImageIntegerSampleCounts", 0x7),
        ("sampledImageDepthSampleCounts", 0x7),
        ("sampledImageStencilSampleCounts", 0x7),
        ("storageImageSampleCounts", 1),
        ("maxSampleMaskWords", 1),
        ("timestampComputeAndGraphics", 1),
        ("maxClipDistances", 8),
        ("maxCombinedClipAndCullDistances", 8),
        ("discreteQueuePriorities", 2),
        ("strictLines", 1),
        ("standardSampleLocations", 1),
    ];
    let arrays = vec![
        ("maxComputeWorkGroupCount", vec![65535, 65535, 65535]),
        ("maxComputeWorkGroupSize", vec![1024, 1024, 64]),
        ("maxViewportDimensions", vec![16384, 16384]),
    ];
    let floats = vec![
        ("maxSamplerLodBias", vec![15.0]),
        ("maxSamplerAnisotropy", vec![16.0]),
        ("viewportBoundsRange", vec![-32768.0, 32767.0]),
        ("minInterpolationOffset", vec![-0.5]),
        ("maxInterpolationOffset", vec![0.4375]),
        ("timestampPeriod", vec![1.0]),
        ("pointSizeRange", vec![1.0, 511.0]),
        ("lineWidthRange", vec![1.0, 1.0]),
        ("pointSizeGranularity", vec![0.0625]),
    ];
    let i32s = vec![("minTexelOffset", -8), ("minTexelGatherOffset", -32)];
    let u64s = vec![
        ("bufferImageGranularity", 1024),
        ("minMemoryMapAlignment", 64),
        ("minTexelBufferOffsetAlignment", 256),
        ("minUniformBufferOffsetAlignment", 256),
        ("minStorageBufferOffsetAlignment", 256),
        ("optimalBufferCopyOffsetAlignment", 256),
        ("optimalBufferCopyRowPitchAlignment", 256),
        ("nonCoherentAtomSize", 64),
    ];
    Limits {
        u32s,
        arrays,
        floats,
        i32s,
        u64s,
    }
}

/// `VkFormatProperties`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormatProperties {
    /// `linearTilingFeatures`.
    pub linear: u32,
    /// `optimalTilingFeatures`.
    pub optimal: u32,
    /// `bufferFeatures`.
    pub buffer: u32,
}

/// Whether a storage image of the format works at the read-write texture tier of Metal.
pub fn storage_image_supported(vk: u32, tier: u8) -> bool {
    match vk {
        98..=100 => tier >= 1,
        107..=109 | 95..=97 | 37 | 41 | 42 | 76 | 74 | 75 | 9 | 13 | 14 => tier >= 2,
        _ => false,
    }
}

/// The features of a format (`vkGetPhysicalDeviceFormatProperties`). An unknown format has none.
pub fn format_properties(vk: u32, caps: &DeviceCaps) -> FormatProperties {
    let mut p = FormatProperties::default();
    let Some(f) = format::lookup(vk) else {
        return p;
    };
    if f.vertex.is_some() {
        p.buffer |= ff::VERTEX_BUFFER;
    }
    if f.pixel.is_none() {
        return p;
    }
    let mut t = ff::SAMPLED_IMAGE | ff::TRANSFER_SRC | ff::TRANSFER_DST | ff::BLIT_SRC;
    if !f.is_int() && f.class != Class::Stencil {
        t |= ff::SAMPLED_IMAGE_FILTER_LINEAR;
    }
    if storage_image_supported(vk, caps.rw_texture_tier) {
        t |= ff::STORAGE_IMAGE;
    }
    if f.is_color() {
        t |= ff::COLOR_ATTACHMENT | ff::BLIT_DST;
        if f.class == Class::Color {
            t |= ff::COLOR_ATTACHMENT_BLEND;
        }
        p.buffer |= ff::UNIFORM_TEXEL_BUFFER;
    }
    if f.has_depth() || f.has_stencil() {
        t |= ff::DEPTH_STENCIL_ATTACHMENT;
    }
    p.optimal = t;
    p.linear = if f.is_color() { t } else { 0 };
    p
}

/// `VkImageFormatProperties`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageFormatProperties {
    /// `maxExtent`.
    pub max_extent: [u32; 3],
    /// `maxMipLevels`.
    pub max_levels: u32,
    /// `maxArrayLayers`.
    pub max_layers: u32,
    /// `sampleCounts`.
    pub sample_counts: u32,
    /// `maxResourceSize`.
    pub max_resource_size: u64,
}

/// `vkGetPhysicalDeviceImageFormatProperties`: what an image of a format, type, tiling, usage and flags may be.
pub fn image_format_properties(
    vk: u32,
    ty: u32,
    tiling_: u32,
    usage: u32,
    flags: u32,
) -> VkResult<ImageFormatProperties> {
    if format::texture_format(vk).is_none() {
        return Err(VkError::FormatNotSupported);
    }
    let max_extent = match ty {
        image_type::D3 => [2048, 2048, 2048],
        image_type::D1 => [16384, 1, 1],
        _ => [16384, 16384, 1],
    };
    let attach = image_usage::COLOR_ATTACHMENT | image_usage::DEPTH_STENCIL_ATTACHMENT;
    let multi = ty == image_type::D2
        && tiling_ == tiling::OPTIMAL
        && flags & image_flags::CUBE_COMPATIBLE == 0
        && usage & attach != 0;
    Ok(ImageFormatProperties {
        max_extent,
        max_levels: 15,
        max_layers: if ty == image_type::D3 { 1 } else { 2048 },
        sample_counts: if multi { 0x7 } else { 1 },
        max_resource_size: 1 << 32,
    })
}

/// `VkSurfaceCapabilitiesKHR`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceCaps {
    /// `minImageCount`.
    pub min_images: u32,
    /// `maxImageCount`.
    pub max_images: u32,
    /// `currentExtent`.
    pub current_extent: (u32, u32),
    /// `minImageExtent`.
    pub min_extent: (u32, u32),
    /// `maxImageExtent`.
    pub max_extent: (u32, u32),
    /// `maxImageArrayLayers`.
    pub max_layers: u32,
    /// `supportedTransforms`: identity, and inherit (Super Mario Galaxy asserts without it; transforms are ignored).
    pub transforms: u32,
    /// `currentTransform`.
    pub current_transform: u32,
    /// `supportedCompositeAlpha`.
    pub composite_alpha: u32,
    /// `supportedUsageFlags`.
    pub usage: u32,
}

/// The capabilities of the surface.
pub fn surface_caps(caps: &DeviceCaps) -> SurfaceCaps {
    SurfaceCaps {
        min_images: 2,
        max_images: 4,
        current_extent: caps.surface_extent,
        min_extent: (1, 1),
        max_extent: (1920, 1080),
        max_layers: 1,
        transforms: 0x1 | 0x100,
        current_transform: 0x1,
        composite_alpha: 0x1,
        usage: image_usage::COLOR_ATTACHMENT
            | image_usage::TRANSFER_SRC
            | image_usage::TRANSFER_DST
            | image_usage::SAMPLED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pose_is_a_tegra_with_vulkan_1_1() {
        assert_eq!(API_VERSION, 0x0040_1000);
        let caps = DeviceCaps::default();
        let f = features(&caps);
        assert!(f[0] && f[18] && !f[4]);
        let one = DeviceCaps {
            max_viewports: 1,
            ..caps
        };
        assert!(!features(&one)[18]);
        assert!(limits(&caps).u32s.contains(&("maxViewports", 16)));
        assert!(!DEVICE_EXTENSIONS.contains(&"VK_AMD_draw_indirect_count"));
    }

    #[test]
    fn format_features_follow_the_class() {
        let caps = DeviceCaps::default();
        let rgba = format_properties(37, &caps);
        assert_ne!(rgba.optimal & ff::COLOR_ATTACHMENT_BLEND, 0);
        assert_ne!(rgba.optimal & ff::STORAGE_IMAGE, 0);
        assert_eq!(rgba.linear, rgba.optimal);
        let depth = format_properties(126, &caps);
        assert_ne!(depth.optimal & ff::DEPTH_STENCIL_ATTACHMENT, 0);
        assert_eq!(depth.linear, 0);
        let uint = format_properties(41, &caps);
        assert_eq!(uint.optimal & ff::SAMPLED_IMAGE_FILTER_LINEAR, 0);
        let vertex_only = format_properties(106, &caps);
        assert_eq!(
            (vertex_only.optimal, vertex_only.buffer),
            (0, ff::VERTEX_BUFFER)
        );
        assert_eq!(format_properties(15, &caps), FormatProperties::default());
        let tier1 = DeviceCaps {
            rw_texture_tier: 1,
            ..caps
        };
        assert_eq!(format_properties(37, &tier1).optimal & ff::STORAGE_IMAGE, 0);
    }

    #[test]
    fn image_format_properties_allow_multisampling_only_for_2d_attachments() {
        let p = image_format_properties(37, 1, 0, image_usage::COLOR_ATTACHMENT, 0);
        assert_eq!(p.map(|p| p.sample_counts), Ok(7));
        let p = image_format_properties(37, 1, 0, image_usage::SAMPLED, 0);
        assert_eq!(p.map(|p| p.sample_counts), Ok(1));
        let p = image_format_properties(37, 0, 0, 0, 0);
        assert_eq!(p.map(|p| p.max_extent), Ok([16384, 1, 1]));
        assert_eq!(
            image_format_properties(106, 1, 0, 0, 0),
            Err(VkError::FormatNotSupported)
        );
    }
}
