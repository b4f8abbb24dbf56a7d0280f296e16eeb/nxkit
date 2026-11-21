//! Decoders of the descriptor structs: set layouts, pools, allocations, writes, copies, update templates and pipeline
//! layouts.

use crate::consts::descriptor_type as dt;
use crate::desc::PushRange;
use crate::layout::{
    copy_descriptor_set as cds, descriptor_buffer_info as dbi, descriptor_image_info as dii,
    descriptor_pool_create_info as dpc, descriptor_set_allocate_info as dsa,
    descriptor_set_layout_binding as dslb, descriptor_set_layout_create_info as dslc,
    descriptor_update_template_create_info as dutc, descriptor_update_template_entry as dute,
    pipeline_layout_create_info as plc, push_constant_range as pcr, write_descriptor_set as wds,
};
use crate::mem::Mem;
use crate::result::VkResult;

use super::{array, handles};

/// One `VkDescriptorSetLayoutBinding`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetLayoutBindingInfo {
    /// `binding`.
    pub binding: u32,
    /// `descriptorType`.
    pub ty: u32,
    /// `descriptorCount`.
    pub count: u32,
    /// `stageFlags`.
    pub stages: u32,
    /// `pImmutableSamplers` (one handle per element), or empty.
    pub immutable: Vec<u64>,
}

/// Reads the bindings of a `VkDescriptorSetLayoutCreateInfo`.
pub fn set_layout(m: &impl Mem, at: u64) -> VkResult<Vec<SetLayoutBindingInfo>> {
    let n = m.u32(at + dslc::BINDING_COUNT)?;
    array(m, m.u64(at + dslc::P_BINDINGS)?, n, dslb::SIZE, |m, b| {
        let ty = m.u32(b + dslb::DESCRIPTOR_TYPE)?;
        let count = m.u32(b + dslb::DESCRIPTOR_COUNT)?;
        let samplers = m.u64(b + dslb::P_IMMUTABLE_SAMPLERS)?;
        let immutable = if ty == dt::SAMPLER || ty == dt::COMBINED_IMAGE_SAMPLER {
            handles(m, samplers, count)?
        } else {
            Vec::new()
        };
        Ok(SetLayoutBindingInfo {
            binding: m.u32(b + dslb::BINDING)?,
            ty,
            count,
            stages: m.u32(b + dslb::STAGE_FLAGS)?,
            immutable,
        })
    })
}

/// Reads a `VkDescriptorPoolCreateInfo`: the flags and `maxSets`.
pub fn descriptor_pool(m: &impl Mem, at: u64) -> VkResult<(u32, u32)> {
    Ok((m.u32(at + dpc::FLAGS)?, m.u32(at + dpc::MAX_SETS)?))
}

/// Reads a `VkDescriptorSetAllocateInfo`: the pool and the set layouts.
pub fn set_allocate(m: &impl Mem, at: u64) -> VkResult<(u64, Vec<u64>)> {
    let n = m.u32(at + dsa::DESCRIPTOR_SET_COUNT)?;
    Ok((
        m.u64(at + dsa::DESCRIPTOR_POOL)?,
        handles(m, m.u64(at + dsa::P_SET_LAYOUTS)?, n)?,
    ))
}

/// What one descriptor element gets written with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteItem {
    /// A `VkDescriptorImageInfo`.
    Image {
        /// `sampler`.
        sampler: u64,
        /// `imageView`.
        view: u64,
    },
    /// A `VkDescriptorBufferInfo`.
    Buffer {
        /// `buffer`.
        buffer: u64,
        /// `offset`.
        offset: u64,
        /// `range` (`VK_WHOLE_SIZE` for the rest).
        range: u64,
    },
    /// A `VkBufferView`.
    TexelBuffer(u64),
    /// A type that the front end does not handle (inline uniform blocks, acceleration structures).
    Unsupported(u32),
}

/// A write of consecutive descriptors (`VkWriteDescriptorSet`, or one entry of an update template).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteInfo {
    /// `dstSet`.
    pub set: u64,
    /// `dstBinding`.
    pub binding: u32,
    /// `dstArrayElement`.
    pub element: u32,
    /// `descriptorType`.
    pub ty: u32,
    /// One item per descriptor.
    pub items: Vec<WriteItem>,
}

/// Reads one item of descriptor type `ty` at `at`, laid out as `VkDescriptorImageInfo`, `VkDescriptorBufferInfo` or
/// `VkBufferView`.
pub fn write_item(m: &impl Mem, ty: u32, at: u64) -> VkResult<WriteItem> {
    Ok(match ty {
        dt::SAMPLER
        | dt::COMBINED_IMAGE_SAMPLER
        | dt::SAMPLED_IMAGE
        | dt::STORAGE_IMAGE
        | dt::INPUT_ATTACHMENT => WriteItem::Image {
            sampler: m.u64(at + dii::SAMPLER)?,
            view: m.u64(at + dii::IMAGE_VIEW)?,
        },
        dt::UNIFORM_TEXEL_BUFFER | dt::STORAGE_TEXEL_BUFFER => WriteItem::TexelBuffer(m.u64(at)?),
        dt::UNIFORM_BUFFER
        | dt::STORAGE_BUFFER
        | dt::UNIFORM_BUFFER_DYNAMIC
        | dt::STORAGE_BUFFER_DYNAMIC => WriteItem::Buffer {
            buffer: m.u64(at + dbi::BUFFER)?,
            offset: m.u64(at + dbi::OFFSET)?,
            range: m.u64(at + dbi::RANGE)?,
        },
        other => WriteItem::Unsupported(other),
    })
}

/// Reads `n` `VkWriteDescriptorSet`.
pub fn writes(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<WriteInfo>> {
    array(m, at, n, wds::SIZE, |m, w| {
        let ty = m.u32(w + wds::DESCRIPTOR_TYPE)?;
        let count = m.u32(w + wds::DESCRIPTOR_COUNT)?;
        let (base, stride) = match ty {
            dt::UNIFORM_TEXEL_BUFFER | dt::STORAGE_TEXEL_BUFFER => {
                (m.u64(w + wds::P_TEXEL_BUFFER_VIEW)?, 8)
            }
            dt::UNIFORM_BUFFER
            | dt::STORAGE_BUFFER
            | dt::UNIFORM_BUFFER_DYNAMIC
            | dt::STORAGE_BUFFER_DYNAMIC => (m.u64(w + wds::P_BUFFER_INFO)?, dbi::SIZE),
            _ => (m.u64(w + wds::P_IMAGE_INFO)?, dii::SIZE),
        };
        let items = if base == 0 {
            Vec::new()
        } else {
            array(m, base, count, stride, |m, a| write_item(m, ty, a))?
        };
        Ok(WriteInfo {
            set: m.u64(w + wds::DST_SET)?,
            binding: m.u32(w + wds::DST_BINDING)?,
            element: m.u32(w + wds::DST_ARRAY_ELEMENT)?,
            ty,
            items,
        })
    })
}

/// A `VkCopyDescriptorSet`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyInfo {
    /// `srcSet`, `srcBinding`, `srcArrayElement`.
    pub src: (u64, u32, u32),
    /// `dstSet`, `dstBinding`, `dstArrayElement`.
    pub dst: (u64, u32, u32),
    /// `descriptorCount`.
    pub count: u32,
}

/// Reads `n` `VkCopyDescriptorSet`.
pub fn copies(m: &impl Mem, at: u64, n: u32) -> VkResult<Vec<CopyInfo>> {
    array(m, at, n, cds::SIZE, |m, c| {
        Ok(CopyInfo {
            src: (
                m.u64(c + cds::SRC_SET)?,
                m.u32(c + cds::SRC_BINDING)?,
                m.u32(c + cds::SRC_ARRAY_ELEMENT)?,
            ),
            dst: (
                m.u64(c + cds::DST_SET)?,
                m.u32(c + cds::DST_BINDING)?,
                m.u32(c + cds::DST_ARRAY_ELEMENT)?,
            ),
            count: m.u32(c + cds::DESCRIPTOR_COUNT)?,
        })
    })
}

/// One `VkDescriptorUpdateTemplateEntry`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TemplateEntry {
    /// `dstBinding`.
    pub binding: u32,
    /// `dstArrayElement`.
    pub element: u32,
    /// `descriptorCount`.
    pub count: u32,
    /// `descriptorType`.
    pub ty: u32,
    /// `offset` into the data.
    pub offset: u64,
    /// `stride` between the items.
    pub stride: u64,
}

/// Reads a `VkDescriptorUpdateTemplateCreateInfo`: the template type and the entries.
pub fn update_template(m: &impl Mem, at: u64) -> VkResult<(u32, Vec<TemplateEntry>)> {
    let n = m.u32(at + dutc::DESCRIPTOR_UPDATE_ENTRY_COUNT)?;
    let entries = array(
        m,
        m.u64(at + dutc::P_DESCRIPTOR_UPDATE_ENTRIES)?,
        n,
        dute::SIZE,
        |m, e| {
            Ok(TemplateEntry {
                binding: m.u32(e + dute::DST_BINDING)?,
                element: m.u32(e + dute::DST_ARRAY_ELEMENT)?,
                count: m.u32(e + dute::DESCRIPTOR_COUNT)?,
                ty: m.u32(e + dute::DESCRIPTOR_TYPE)?,
                offset: m.u64(e + dute::OFFSET)?,
                stride: m.u64(e + dute::STRIDE)?,
            })
        },
    )?;
    Ok((m.u32(at + dutc::TEMPLATE_TYPE)?, entries))
}

/// The writes that an update template makes to `set` from the game's `data`.
pub fn template_writes(
    m: &impl Mem,
    set: u64,
    entries: &[TemplateEntry],
    data: u64,
) -> VkResult<Vec<WriteInfo>> {
    entries
        .iter()
        .map(|e| {
            let items = (0..u64::from(crate::mem::check_count(e.count)?))
                .map(|j| write_item(m, e.ty, data + e.offset + j * e.stride))
                .collect::<VkResult<Vec<_>>>()?;
            Ok(WriteInfo {
                set,
                binding: e.binding,
                element: e.element,
                ty: e.ty,
                items,
            })
        })
        .collect()
}

/// Reads a `VkPipelineLayoutCreateInfo`: the set layouts and the push constant ranges.
pub fn pipeline_layout(m: &impl Mem, at: u64) -> VkResult<(Vec<u64>, Vec<PushRange>)> {
    let sets = handles(
        m,
        m.u64(at + plc::P_SET_LAYOUTS)?,
        m.u32(at + plc::SET_LAYOUT_COUNT)?,
    )?;
    let n = m.u32(at + plc::PUSH_CONSTANT_RANGE_COUNT)?;
    let ranges = array(
        m,
        m.u64(at + plc::P_PUSH_CONSTANT_RANGES)?,
        n,
        pcr::SIZE,
        |m, r| {
            Ok(PushRange {
                stages: m.u32(r + pcr::STAGE_FLAGS)?,
                offset: m.u32(r + pcr::OFFSET)?,
                size: m.u32(r + pcr::SIZE_)?,
            })
        },
    )?;
    Ok((sets, ranges))
}
