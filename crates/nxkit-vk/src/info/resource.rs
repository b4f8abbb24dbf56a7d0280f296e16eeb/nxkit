//! Decoders of the instance, device, memory, buffer, image, view and sampler structs.

use crate::desc::{SamplerDesc, SubresourceRange};
use crate::layout::{
    application_info as ai, bind_buffer_memory_info as bbm, bind_image_memory_info as bim,
    buffer_create_info as bc, buffer_view_create_info as bvc, component_mapping as cm,
    device_create_info as dc, device_queue_create_info as dq, image_create_info as ic,
    image_subresource_range as isr, image_view_create_info as ivc, instance_create_info as inc,
    memory_allocate_info as ma, sampler_create_info as sc,
};
use crate::mem::Mem;
use crate::result::VkResult;

use super::{array, extent3};

/// `VkInstanceCreateInfo`, for the log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstanceInfo {
    /// `pApplicationName`.
    pub application: String,
    /// `pEngineName`.
    pub engine: String,
    /// `apiVersion`.
    pub api_version: u32,
    /// The enabled extensions.
    pub extensions: Vec<String>,
    /// The number of enabled layers.
    pub layers: u32,
}

/// Reads a `VkInstanceCreateInfo`.
pub fn instance(m: &impl Mem, at: u64) -> VkResult<InstanceInfo> {
    let app = m.u64(at + inc::P_APPLICATION_INFO)?;
    let (application, engine, api_version) = if app == 0 {
        (String::new(), String::new(), 0)
    } else {
        (
            m.cstr(m.u64(app + ai::P_APPLICATION_NAME)?, 256)?,
            m.cstr(m.u64(app + ai::P_ENGINE_NAME)?, 256)?,
            m.u32(app + ai::API_VERSION)?,
        )
    };
    Ok(InstanceInfo {
        application,
        engine,
        api_version,
        extensions: m.cstrs(
            m.u64(at + inc::PP_ENABLED_EXTENSION_NAMES)?,
            m.u32(at + inc::ENABLED_EXTENSION_COUNT)?,
        )?,
        layers: m.u32(at + inc::ENABLED_LAYER_COUNT)?,
    })
}

/// `VkDeviceCreateInfo`, for the log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The queues over all queue create infos.
    pub queues: u32,
    /// The enabled extensions.
    pub extensions: Vec<String>,
}

/// Reads a `VkDeviceCreateInfo`.
pub fn device(m: &impl Mem, at: u64) -> VkResult<DeviceInfo> {
    let n = m.u32(at + dc::QUEUE_CREATE_INFO_COUNT)?;
    let counts = array(
        m,
        m.u64(at + dc::P_QUEUE_CREATE_INFOS)?,
        n,
        dq::SIZE,
        |m, a| m.u32(a + dq::QUEUE_COUNT),
    )?;
    Ok(DeviceInfo {
        queues: counts.iter().sum(),
        extensions: m.cstrs(
            m.u64(at + dc::PP_ENABLED_EXTENSION_NAMES)?,
            m.u32(at + dc::ENABLED_EXTENSION_COUNT)?,
        )?,
    })
}

/// Reads a `VkMemoryAllocateInfo`: the size and the memory type index.
pub fn memory_allocate(m: &impl Mem, at: u64) -> VkResult<(u64, u32)> {
    Ok((
        m.u64(at + ma::ALLOCATION_SIZE)?,
        m.u32(at + ma::MEMORY_TYPE_INDEX)?,
    ))
}

/// `VkBufferCreateInfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferInfo {
    /// `flags`.
    pub flags: u32,
    /// `size`.
    pub size: u64,
    /// `usage`.
    pub usage: u32,
}

/// Reads a `VkBufferCreateInfo`.
pub fn buffer(m: &impl Mem, at: u64) -> VkResult<BufferInfo> {
    Ok(BufferInfo {
        flags: m.u32(at + bc::FLAGS)?,
        size: m.u64(at + bc::SIZE_)?,
        usage: m.u32(at + bc::USAGE)?,
    })
}

/// `VkBufferViewCreateInfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferViewInfo {
    /// `buffer`.
    pub buffer: u64,
    /// `format`.
    pub format: u32,
    /// `offset`.
    pub offset: u64,
    /// `range` (`VK_WHOLE_SIZE` for the rest of the buffer).
    pub range: u64,
}

/// Reads a `VkBufferViewCreateInfo`.
pub fn buffer_view(m: &impl Mem, at: u64) -> VkResult<BufferViewInfo> {
    Ok(BufferViewInfo {
        buffer: m.u64(at + bvc::BUFFER)?,
        format: m.u32(at + bvc::FORMAT)?,
        offset: m.u64(at + bvc::OFFSET)?,
        range: m.u64(at + bvc::RANGE)?,
    })
}

/// `VkImageCreateInfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageInfo {
    /// `flags`.
    pub flags: u32,
    /// `imageType`.
    pub image_type: u32,
    /// `format`.
    pub format: u32,
    /// `extent`.
    pub extent: [u32; 3],
    /// `mipLevels`.
    pub levels: u32,
    /// `arrayLayers`.
    pub layers: u32,
    /// `samples` (`VkSampleCountFlagBits`, which is the count).
    pub samples: u32,
    /// `tiling`.
    pub tiling: u32,
    /// `usage`.
    pub usage: u32,
}

/// Reads a `VkImageCreateInfo`.
pub fn image(m: &impl Mem, at: u64) -> VkResult<ImageInfo> {
    Ok(ImageInfo {
        flags: m.u32(at + ic::FLAGS)?,
        image_type: m.u32(at + ic::IMAGE_TYPE)?,
        format: m.u32(at + ic::FORMAT)?,
        extent: extent3(m, at + ic::EXTENT)?,
        levels: m.u32(at + ic::MIP_LEVELS)?,
        layers: m.u32(at + ic::ARRAY_LAYERS)?,
        samples: m.u32(at + ic::SAMPLES)?,
        tiling: m.u32(at + ic::TILING)?,
        usage: m.u32(at + ic::USAGE)?,
    })
}

/// Reads a `VkImageSubresourceRange`.
pub fn subresource_range(m: &impl Mem, at: u64) -> VkResult<SubresourceRange> {
    Ok(SubresourceRange {
        aspect: m.u32(at + isr::ASPECT_MASK)?,
        base_level: m.u32(at + isr::BASE_MIP_LEVEL)?,
        levels: m.u32(at + isr::LEVEL_COUNT)?,
        base_layer: m.u32(at + isr::BASE_ARRAY_LAYER)?,
        layers: m.u32(at + isr::LAYER_COUNT)?,
    })
}

/// `VkImageViewCreateInfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageViewInfo {
    /// `image`.
    pub image: u64,
    /// `viewType`.
    pub view_type: u32,
    /// `format`.
    pub format: u32,
    /// `components` (`VkComponentSwizzle` r, g, b, a).
    pub components: [u32; 4],
    /// `subresourceRange`.
    pub range: SubresourceRange,
}

/// Reads a `VkImageViewCreateInfo`.
pub fn image_view(m: &impl Mem, at: u64) -> VkResult<ImageViewInfo> {
    let c = at + ivc::COMPONENTS;
    Ok(ImageViewInfo {
        image: m.u64(at + ivc::IMAGE)?,
        view_type: m.u32(at + ivc::VIEW_TYPE)?,
        format: m.u32(at + ivc::FORMAT)?,
        components: [
            m.u32(c + cm::R)?,
            m.u32(c + cm::G)?,
            m.u32(c + cm::B)?,
            m.u32(c + cm::A)?,
        ],
        range: subresource_range(m, at + ivc::SUBRESOURCE_RANGE)?,
    })
}

/// Reads a `VkSamplerCreateInfo` into the sampler description, with the clamps of the device.
pub fn sampler(m: &impl Mem, at: u64) -> VkResult<SamplerDesc> {
    let aniso = m.bool32(at + sc::ANISOTROPY_ENABLE)?;
    let compare = m.bool32(at + sc::COMPARE_ENABLE)?;
    Ok(SamplerDesc {
        mag_filter: m.u32(at + sc::MAG_FILTER)?,
        min_filter: m.u32(at + sc::MIN_FILTER)?,
        mipmap_mode: m.u32(at + sc::MIPMAP_MODE)?,
        address: [
            m.u32(at + sc::ADDRESS_MODE_U)?,
            m.u32(at + sc::ADDRESS_MODE_V)?,
            m.u32(at + sc::ADDRESS_MODE_W)?,
        ],
        lod_bias: m.f32(at + sc::MIP_LOD_BIAS)?.clamp(-15.0, 15.0),
        max_anisotropy: if aniso {
            m.f32(at + sc::MAX_ANISOTROPY)?.clamp(1.0, 16.0)
        } else {
            1.0
        },
        compare: if compare {
            Some(m.u32(at + sc::COMPARE_OP)?)
        } else {
            None
        },
        min_lod: m.f32(at + sc::MIN_LOD)?,
        max_lod: m.f32(at + sc::MAX_LOD)?,
        border_color: m.u32(at + sc::BORDER_COLOR)?,
        unnormalized: m.bool32(at + sc::UNNORMALIZED_COORDINATES)?,
    })
}

/// One memory binding of `vkBind*Memory2`: the buffer or image, the memory and the offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BindInfo {
    /// The buffer or the image.
    pub target: u64,
    /// The memory.
    pub memory: u64,
    /// The offset in the memory.
    pub offset: u64,
}

/// Reads `n` `VkBindBufferMemoryInfo`.
pub fn bind_buffers(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<BindInfo>> {
    array(m, at, n, bbm::SIZE, |m, a| {
        Ok(BindInfo {
            target: m.u64(a + bbm::BUFFER)?,
            memory: m.u64(a + bbm::MEMORY)?,
            offset: m.u64(a + bbm::MEMORY_OFFSET)?,
        })
    })
}

/// Reads `n` `VkBindImageMemoryInfo`.
pub fn bind_images(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<BindInfo>> {
    array(m, at, n, bim::SIZE, |m, a| {
        Ok(BindInfo {
            target: m.u64(a + bim::IMAGE)?,
            memory: m.u64(a + bim::MEMORY)?,
            offset: m.u64(a + bim::MEMORY_OFFSET)?,
        })
    })
}

/// The buffer or image of a `VkBufferMemoryRequirementsInfo2` or `VkImageMemoryRequirementsInfo2`.
pub fn requirements_target(m: &impl Mem, at: u64) -> VkResult<u64> {
    m.u64(at + 16)
}
