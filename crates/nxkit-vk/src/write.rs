//! Writers of the structs that the host fills in for the game: the device description of [`crate::device`], memory
//! requirements, surface data, and the count-and-array protocol of the `vkEnumerate*` and `vkGet*` calls.
//!
//! A writer zeroes the struct first, so no byte keeps what the game left there. For a `*2` struct it leaves `sType` and
//! `pNext` as the game set them and fills the extension structs of the chain that it knows.

use crate::consts::stype;
use crate::device::{self, DeviceCaps, FormatProperties, ImageFormatProperties, SurfaceCaps};
use crate::layout::{
    LIMIT_FIELDS, extension_properties as ep, format_properties as fp,
    image_format_properties as ifp, memory_dedicated_requirements as mdr,
    memory_requirements as mr, physical_device_group_properties as pdg,
    physical_device_memory_properties as pdm, physical_device_properties as pdp,
    queue_family_properties as qf, subresource_layout as sl, surface_capabilities_khr as sc,
    surface_format_khr as sf,
};
use crate::mem::Mem;
use crate::result::{Status, VkResult};

fn zero(m: &impl Mem, addr: u64, len: u64) -> VkResult<()> {
    m.write(addr, &vec![0u8; len as usize])
}

/// The count-and-array protocol: with `out` null, writes the number of items to `count`. Otherwise, it writes at most
/// `*count` items, each with `put(index, address)`, sets `*count` to the number written and gives
/// [`Status::Incomplete`] when that is fewer than all.
pub fn enumerate<M: Mem>(
    m: &M,
    count: u64,
    out: u64,
    total: u32,
    stride: u64,
    mut put: impl FnMut(&M, u32, u64) -> VkResult<()>,
) -> VkResult<Status> {
    if out == 0 {
        m.put_u32(count, total)?;
        return Ok(Status::Success);
    }
    let k = m.u32(count)?.min(total);
    for i in 0..k {
        put(m, i, out + u64::from(i) * stride)?;
    }
    m.put_u32(count, k)?;
    Ok(if k < total {
        Status::Incomplete
    } else {
        Status::Success
    })
}

/// `VkExtensionProperties` for a list of names (spec version 1 each).
pub fn extensions(m: &impl Mem, count: u64, out: u64, names: &[&str]) -> VkResult<Status> {
    enumerate(m, count, out, names.len() as u32, ep::SIZE, |m, i, at| {
        zero(m, at, ep::SIZE)?;
        let name = names.get(i as usize).copied().unwrap_or("");
        m.write(
            at + ep::EXTENSION_NAME,
            &name.as_bytes()[..name.len().min(255)],
        )?;
        m.put_u32(at + ep::SPEC_VERSION, 1)
    })
}

/// An empty list: zero layers and the like.
pub fn none(m: &impl Mem, count: u64) -> VkResult<Status> {
    m.put_u32(count, 0)?;
    Ok(Status::Success)
}

/// `VkPhysicalDeviceProperties` at `addr`.
pub fn properties(m: &impl Mem, addr: u64, caps: &DeviceCaps) -> VkResult<()> {
    zero(m, addr, pdp::SIZE)?;
    let id = device::IDENTITY;
    m.put_u32(addr + pdp::API_VERSION, device::API_VERSION)?;
    m.put_u32(addr + pdp::DRIVER_VERSION, id.driver_version)?;
    m.put_u32(addr + pdp::VENDOR_ID, id.vendor_id)?;
    m.put_u32(addr + pdp::DEVICE_ID, id.device_id)?;
    m.put_u32(addr + pdp::DEVICE_TYPE, id.device_type)?;
    m.write(addr + pdp::DEVICE_NAME, id.name.as_bytes())?;
    let base = addr + pdp::LIMITS;
    let off = |name: &str| {
        LIMIT_FIELDS
            .iter()
            .find(|f| f.0 == name)
            .map(|f| base + f.1)
    };
    let l = device::limits(caps);
    for (n, v) in &l.u32s {
        if let Some(a) = off(n) {
            m.put_u32(a, *v)?;
        }
    }
    for (n, vs) in &l.arrays {
        if let Some(a) = off(n) {
            for (i, v) in vs.iter().enumerate() {
                m.put_u32(a + 4 * i as u64, *v)?;
            }
        }
    }
    for (n, vs) in &l.floats {
        if let Some(a) = off(n) {
            for (i, v) in vs.iter().enumerate() {
                m.put_u32(a + 4 * i as u64, v.to_bits())?;
            }
        }
    }
    for (n, v) in &l.i32s {
        if let Some(a) = off(n) {
            m.put_u32(a, *v as u32)?;
        }
    }
    for (n, v) in &l.u64s {
        if let Some(a) = off(n) {
            m.put_u64(a, *v)?;
        }
    }
    Ok(())
}

/// `VkPhysicalDeviceFeatures` at `addr`: 55 `VkBool32`.
pub fn features(m: &impl Mem, addr: u64, caps: &DeviceCaps) -> VkResult<()> {
    let mut b = Vec::with_capacity(55 * 4);
    for f in device::features(caps) {
        b.extend_from_slice(&u32::from(f).to_le_bytes());
    }
    m.write(addr, &b)
}

/// `VkPhysicalDeviceMemoryProperties` at `addr`.
pub fn memory_properties(m: &impl Mem, addr: u64) -> VkResult<()> {
    zero(m, addr, pdm::SIZE)?;
    m.put_u32(
        addr + pdm::MEMORY_TYPE_COUNT,
        device::MEMORY_TYPES.len() as u32,
    )?;
    for (i, flags) in device::MEMORY_TYPES.iter().enumerate() {
        // VkMemoryType: propertyFlags, heapIndex.
        m.put_u32(addr + pdm::MEMORY_TYPES + 8 * i as u64, *flags)?;
    }
    m.put_u32(addr + pdm::MEMORY_HEAP_COUNT, 1)?;
    // VkMemoryHeap: size, flags (VK_MEMORY_HEAP_DEVICE_LOCAL_BIT).
    m.put_u64(addr + pdm::MEMORY_HEAPS, device::HEAP_SIZE)?;
    m.put_u32(addr + pdm::MEMORY_HEAPS + 8, 1)
}

/// The queue families (one).
pub fn queue_families(m: &impl Mem, count: u64, out: u64) -> VkResult<Status> {
    enumerate(m, count, out, 1, qf::SIZE, |m, _, at| {
        let q = device::QUEUE_FAMILY;
        m.put_u32(at + qf::QUEUE_FLAGS, q.flags)?;
        m.put_u32(at + qf::QUEUE_COUNT, q.count)?;
        m.put_u32(at + qf::TIMESTAMP_VALID_BITS, q.timestamp_bits)?;
        for (i, g) in q.granularity.iter().enumerate() {
            m.put_u32(at + qf::MIN_IMAGE_TRANSFER_GRANULARITY + 4 * i as u64, *g)?;
        }
        Ok(())
    })
}

/// `VkFormatProperties` at `addr`.
pub fn format_properties(m: &impl Mem, addr: u64, p: FormatProperties) -> VkResult<()> {
    m.put_u32(addr + fp::LINEAR_TILING_FEATURES, p.linear)?;
    m.put_u32(addr + fp::OPTIMAL_TILING_FEATURES, p.optimal)?;
    m.put_u32(addr + fp::BUFFER_FEATURES, p.buffer)
}

/// `VkImageFormatProperties` at `addr`.
pub fn image_format_properties(m: &impl Mem, addr: u64, p: &ImageFormatProperties) -> VkResult<()> {
    zero(m, addr, ifp::SIZE)?;
    for (i, e) in p.max_extent.iter().enumerate() {
        m.put_u32(addr + ifp::MAX_EXTENT + 4 * i as u64, *e)?;
    }
    m.put_u32(addr + ifp::MAX_MIP_LEVELS, p.max_levels)?;
    m.put_u32(addr + ifp::MAX_ARRAY_LAYERS, p.max_layers)?;
    m.put_u32(addr + ifp::SAMPLE_COUNTS, p.sample_counts)?;
    m.put_u64(addr + ifp::MAX_RESOURCE_SIZE, p.max_resource_size)
}

/// Memory requirements of a buffer or an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    /// `size`.
    pub size: u64,
    /// `alignment`.
    pub alignment: u64,
    /// `memoryTypeBits`.
    pub type_bits: u32,
}

/// `VkMemoryRequirements` at `addr`.
pub fn requirements(m: &impl Mem, addr: u64, r: Requirements) -> VkResult<()> {
    zero(m, addr, mr::SIZE)?;
    m.put_u64(addr + mr::SIZE_, r.size)?;
    m.put_u64(addr + mr::ALIGNMENT, r.alignment)?;
    m.put_u32(addr + mr::MEMORY_TYPE_BITS, r.type_bits)
}

/// `VkMemoryRequirements2` at `addr`: the requirements at 16, and `VkMemoryDedicatedRequirements` in the chain says
/// that no dedicated allocation is needed or preferred.
pub fn requirements2(m: &impl Mem, addr: u64, r: Requirements) -> VkResult<()> {
    requirements(m, addr + 16, r)?;
    for (ty, at) in m.chain(m.u64(addr + 8)?)? {
        if ty == stype::MEMORY_DEDICATED_REQUIREMENTS {
            m.put_u32(at + mdr::PREFERS_DEDICATED_ALLOCATION, 0)?;
            m.put_u32(at + mdr::REQUIRES_DEDICATED_ALLOCATION, 0)?;
        }
    }
    Ok(())
}

/// `VkSurfaceCapabilitiesKHR` at `addr`.
pub fn surface_caps(m: &impl Mem, addr: u64, c: &SurfaceCaps) -> VkResult<()> {
    zero(m, addr, sc::SIZE)?;
    m.put_u32(addr + sc::MIN_IMAGE_COUNT, c.min_images)?;
    m.put_u32(addr + sc::MAX_IMAGE_COUNT, c.max_images)?;
    for (off, (w, h)) in [
        (sc::CURRENT_EXTENT, c.current_extent),
        (sc::MIN_IMAGE_EXTENT, c.min_extent),
        (sc::MAX_IMAGE_EXTENT, c.max_extent),
    ] {
        m.put_u32(addr + off, w)?;
        m.put_u32(addr + off + 4, h)?;
    }
    m.put_u32(addr + sc::MAX_IMAGE_ARRAY_LAYERS, c.max_layers)?;
    m.put_u32(addr + sc::SUPPORTED_TRANSFORMS, c.transforms)?;
    m.put_u32(addr + sc::CURRENT_TRANSFORM, c.current_transform)?;
    m.put_u32(addr + sc::SUPPORTED_COMPOSITE_ALPHA, c.composite_alpha)?;
    m.put_u32(addr + sc::SUPPORTED_USAGE_FLAGS, c.usage)
}

/// The surface formats (`VkSurfaceFormatKHR`).
pub fn surface_formats(m: &impl Mem, count: u64, out: u64) -> VkResult<Status> {
    let f = device::SURFACE_FORMATS;
    enumerate(m, count, out, f.len() as u32, sf::SIZE, |m, i, at| {
        m.put_u32(at + sf::FORMAT, f.get(i as usize).copied().unwrap_or(0))?;
        m.put_u32(at + sf::COLOR_SPACE, 0)
    })
}

/// A list of `u32` values (present modes).
pub fn u32_list(m: &impl Mem, count: u64, out: u64, values: &[u32]) -> VkResult<Status> {
    enumerate(m, count, out, values.len() as u32, 4, |m, i, at| {
        m.put_u32(at, values.get(i as usize).copied().unwrap_or(0))
    })
}

/// A list of `u64` values (handles).
pub fn u64_list(m: &impl Mem, count: u64, out: u64, values: &[u64]) -> VkResult<Status> {
    enumerate(m, count, out, values.len() as u32, 8, |m, i, at| {
        m.put_u64(at, values.get(i as usize).copied().unwrap_or(0))
    })
}

/// One `VkPhysicalDeviceGroupProperties` with the one physical device.
pub fn device_groups(m: &impl Mem, count: u64, out: u64, physical: u64) -> VkResult<Status> {
    enumerate(m, count, out, 1, pdg::SIZE, |m, _, at| {
        m.put_u32(at + pdg::PHYSICAL_DEVICE_COUNT, 1)?;
        m.put_u64(at + pdg::PHYSICAL_DEVICES, physical)?;
        m.put_u32(at + pdg::SUBSET_ALLOCATION, 0)
    })
}

/// `VkSubresourceLayout`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubresourceLayout {
    /// `offset`.
    pub offset: u64,
    /// `size`.
    pub size: u64,
    /// `rowPitch`.
    pub row_pitch: u64,
    /// `arrayPitch`.
    pub array_pitch: u64,
    /// `depthPitch`.
    pub depth_pitch: u64,
}

/// `VkSubresourceLayout` at `addr`.
pub fn subresource_layout(m: &impl Mem, addr: u64, l: SubresourceLayout) -> VkResult<()> {
    m.put_u64(addr + sl::OFFSET, l.offset)?;
    m.put_u64(addr + sl::SIZE_, l.size)?;
    m.put_u64(addr + sl::ROW_PITCH, l.row_pitch)?;
    m.put_u64(addr + sl::ARRAY_PITCH, l.array_pitch)?;
    m.put_u64(addr + sl::DEPTH_PITCH, l.depth_pitch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{StructBytes, TestMemory};

    #[test]
    fn enumerate_counts_then_fills_and_reports_incomplete() {
        let m = TestMemory::new(0x1_0000, 4096);
        let count = m.alloc(4);
        assert_eq!(
            extensions(&m, count, 0, device::DEVICE_EXTENSIONS),
            Ok(Status::Success)
        );
        assert_eq!(m.u32(count), Ok(6));
        let out = m.alloc(ep::SIZE * 6);
        let _ = m.put_u32(count, 2);
        assert_eq!(
            extensions(&m, count, out, device::DEVICE_EXTENSIONS),
            Ok(Status::Incomplete)
        );
        assert_eq!(m.u32(count), Ok(2));
        assert_eq!(
            m.cstr(out + ep::SIZE, 256),
            Ok("VK_KHR_descriptor_update_template".into())
        );
        assert_eq!(m.u32(out + ep::SPEC_VERSION), Ok(1));
    }

    #[test]
    fn properties_carry_the_name_and_the_limits() {
        let m = TestMemory::new(0x1_0000, 4096);
        let at = m.alloc(pdp::SIZE);
        assert!(properties(&m, at, &DeviceCaps::default()).is_ok());
        assert_eq!(m.u32(at + pdp::VENDOR_ID), Ok(0x10de));
        assert_eq!(
            m.cstr(at + pdp::DEVICE_NAME, 256),
            Ok("NVIDIA Tegra X1 (nvgpu)".into())
        );
        let l = at + pdp::LIMITS;
        assert_eq!(
            m.u32(l + crate::layout::physical_device_limits::MAX_PUSH_CONSTANTS_SIZE),
            Ok(256)
        );
        assert_eq!(
            m.u64(l + crate::layout::physical_device_limits::NON_COHERENT_ATOM_SIZE),
            Ok(64)
        );
        assert_eq!(
            m.f32(l + crate::layout::physical_device_limits::TIMESTAMP_PERIOD),
            Ok(1.0)
        );
        assert_eq!(
            m.i32(l + crate::layout::physical_device_limits::MIN_TEXEL_OFFSET),
            Ok(-8)
        );
    }

    #[test]
    fn requirements2_clears_the_dedicated_request() {
        let m = TestMemory::new(0x1_0000, 4096);
        let ded = m.put(
            &StructBytes::new(mdr::SIZE)
                .u32(0, stype::MEMORY_DEDICATED_REQUIREMENTS)
                .u32(16, 1)
                .u32(20, 1)
                .0,
        );
        let r2 = m.put(&StructBytes::new(40).u64(8, ded).0);
        let r = Requirements {
            size: 512,
            alignment: 256,
            type_bits: 7,
        };
        assert!(requirements2(&m, r2, r).is_ok());
        assert_eq!(m.u64(r2 + 16), Ok(512));
        assert_eq!(m.u32(r2 + 16 + mr::MEMORY_TYPE_BITS), Ok(7));
        assert_eq!(m.u32(ded + 16), Ok(0));
        assert_eq!(m.u32(ded + 20), Ok(0));
    }

    #[test]
    fn memory_properties_have_three_types_on_one_heap() {
        let m = TestMemory::new(0x1_0000, 4096);
        let at = m.alloc(pdm::SIZE);
        assert!(memory_properties(&m, at).is_ok());
        assert_eq!(m.u32(at), Ok(3));
        assert_eq!(m.u32(at + pdm::MEMORY_TYPES + 8), Ok(7));
        assert_eq!(m.u64(at + pdm::MEMORY_HEAPS), Ok(3 << 30));
    }
}
