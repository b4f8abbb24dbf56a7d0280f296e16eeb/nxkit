//! Device memory, buffers, buffer views, images, image views and samplers.
//!
//! Memory is host-coherent (one heap of shared storage), so a flush or an invalidate of a mapped range has nothing to
//! do and an unmap keeps the mapping.

use nxkit_vk::mem::Mem;
use nxkit_vk::result::Status;
use nxkit_vk::{info, write};

use super::abi::{M, call, code, handle_out, ok, procs};

extern "C" fn allocate_memory(_device: u64, ai: u64, _alloc: u64, out: u64) -> i32 {
    code("vkAllocateMemory", |vk| {
        let (size, type_index) = info::memory_allocate(&M, ai)?;
        handle_out(out, vk.allocate_memory(size, type_index))
    })
}

extern "C" fn free_memory(_device: u64, memory: u64, _alloc: u64) {
    call("vkFreeMemory", |vk| {
        vk.free_memory(memory);
        Ok(())
    })
}

extern "C" fn map_memory(
    _device: u64,
    memory: u64,
    offset: u64,
    _size: u64,
    _flags: u32,
    out: u64,
) -> i32 {
    code("vkMapMemory", |vk| {
        let host = vk.map_memory(memory, offset)?;
        M.put_u64(out, host)?;
        Ok(Status::Success)
    })
}

extern "C" fn unmap_memory(_device: u64, _memory: u64) {
    call("vkUnmapMemory", |_| Ok(()))
}

/// `vkFlushMappedMemoryRanges` and `vkInvalidateMappedMemoryRanges`: the memory is coherent.
extern "C" fn mapped_ranges(_device: u64, _count: u32, _ranges: u64) -> i32 {
    code("vkFlushMappedMemoryRanges", |_| Ok(Status::Success))
}

extern "C" fn create_buffer(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateBuffer", |vk| {
        handle_out(out, Ok(vk.create_buffer(&info::buffer(&M, ci)?)))
    })
}

extern "C" fn destroy_buffer(_device: u64, buffer: u64, _alloc: u64) {
    call("vkDestroyBuffer", |vk| {
        vk.destroy_buffer(buffer);
        Ok(())
    })
}

extern "C" fn get_buffer_memory_requirements(_device: u64, buffer: u64, out: u64) {
    call("vkGetBufferMemoryRequirements", |vk| {
        write::requirements(&M, out, vk.buffer_requirements(buffer)?)
    })
}

extern "C" fn get_buffer_memory_requirements2(_device: u64, ri: u64, out: u64) {
    call("vkGetBufferMemoryRequirements2", |vk| {
        let buffer = info::requirements_target(&M, ri)?;
        write::requirements2(&M, out, vk.buffer_requirements(buffer)?)
    })
}

extern "C" fn bind_buffer_memory(_device: u64, buffer: u64, memory: u64, offset: u64) -> i32 {
    code("vkBindBufferMemory", |vk| {
        ok(vk.bind_buffer_memory(buffer, memory, offset))
    })
}

extern "C" fn bind_buffer_memory2(_device: u64, count: u32, infos: u64) -> i32 {
    code("vkBindBufferMemory2", |vk| {
        for b in info::bind_buffers(&M, infos, count)? {
            vk.bind_buffer_memory(b.target, b.memory, b.offset)?;
        }
        Ok(Status::Success)
    })
}

extern "C" fn create_buffer_view(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateBufferView", |vk| {
        handle_out(out, vk.create_buffer_view(&info::buffer_view(&M, ci)?))
    })
}

extern "C" fn destroy_buffer_view(_device: u64, view: u64, _alloc: u64) {
    call("vkDestroyBufferView", |vk| {
        vk.destroy_buffer_view(view);
        Ok(())
    })
}

extern "C" fn create_image(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateImage", |vk| {
        handle_out(out, vk.create_image(&info::image(&M, ci)?))
    })
}

extern "C" fn destroy_image(_device: u64, image: u64, _alloc: u64) {
    call("vkDestroyImage", |vk| {
        vk.destroy_image(image);
        Ok(())
    })
}

extern "C" fn get_image_memory_requirements(_device: u64, image: u64, out: u64) {
    call("vkGetImageMemoryRequirements", |vk| {
        write::requirements(&M, out, vk.image_requirements(image)?)
    })
}

extern "C" fn get_image_memory_requirements2(_device: u64, ri: u64, out: u64) {
    call("vkGetImageMemoryRequirements2", |vk| {
        let image = info::requirements_target(&M, ri)?;
        write::requirements2(&M, out, vk.image_requirements(image)?)
    })
}

/// `vkGetImageSparseMemoryRequirements`: no image is sparse.
extern "C" fn get_image_sparse_memory_requirements(
    _device: u64,
    _image: u64,
    count: u64,
    _out: u64,
) {
    call("vkGetImageSparseMemoryRequirements", |_| {
        M.put_u32(count, 0)
    })
}

extern "C" fn bind_image_memory(_device: u64, image: u64, memory: u64, offset: u64) -> i32 {
    code("vkBindImageMemory", |vk| {
        ok(vk.bind_image_memory(image, memory, offset))
    })
}

extern "C" fn bind_image_memory2(_device: u64, count: u32, infos: u64) -> i32 {
    code("vkBindImageMemory2", |vk| {
        for b in info::bind_images(&M, infos, count)? {
            vk.bind_image_memory(b.target, b.memory, b.offset)?;
        }
        Ok(Status::Success)
    })
}

/// `vkGetImageSubresourceLayout(device, image, const VkImageSubresource *, VkSubresourceLayout *)`: the subresource
/// is the aspect, the level at 4 and the layer at 8.
extern "C" fn get_image_subresource_layout(_device: u64, image: u64, sub: u64, out: u64) {
    call("vkGetImageSubresourceLayout", |vk| {
        let l = vk.subresource_layout(image, M.u32(sub + 4)?, M.u32(sub + 8)?)?;
        write::subresource_layout(&M, out, l)
    })
}

extern "C" fn create_image_view(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateImageView", |vk| {
        handle_out(out, vk.create_image_view(&info::image_view(&M, ci)?))
    })
}

extern "C" fn destroy_image_view(_device: u64, view: u64, _alloc: u64) {
    call("vkDestroyImageView", |vk| {
        vk.destroy_image_view(view);
        Ok(())
    })
}

extern "C" fn create_sampler(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateSampler", |vk| {
        handle_out(out, vk.create_sampler(&info::sampler(&M, ci)?))
    })
}

extern "C" fn destroy_sampler(_device: u64, sampler: u64, _alloc: u64) {
    call("vkDestroySampler", |vk| {
        vk.destroy_sampler(sampler);
        Ok(())
    })
}

pub(super) fn procs() -> Vec<super::Proc> {
    procs![
        "vkAllocateMemory" => allocate_memory,
        "vkFreeMemory" => free_memory [big],
        "vkMapMemory" => map_memory [big],
        "vkUnmapMemory" => unmap_memory,
        "vkFlushMappedMemoryRanges" => mapped_ranges,
        "vkInvalidateMappedMemoryRanges" => mapped_ranges,
        "vkCreateBuffer" => create_buffer,
        "vkDestroyBuffer" => destroy_buffer,
        "vkGetBufferMemoryRequirements" => get_buffer_memory_requirements,
        "vkGetBufferMemoryRequirements2" => get_buffer_memory_requirements2,
        "vkGetBufferMemoryRequirements2KHR" => get_buffer_memory_requirements2,
        "vkBindBufferMemory" => bind_buffer_memory [big],
        "vkBindBufferMemory2" => bind_buffer_memory2 [big],
        "vkBindBufferMemory2KHR" => bind_buffer_memory2 [big],
        "vkCreateBufferView" => create_buffer_view [big],
        "vkDestroyBufferView" => destroy_buffer_view [big],
        "vkCreateImage" => create_image [big],
        "vkDestroyImage" => destroy_image [big],
        "vkGetImageMemoryRequirements" => get_image_memory_requirements,
        "vkGetImageMemoryRequirements2" => get_image_memory_requirements2,
        "vkGetImageMemoryRequirements2KHR" => get_image_memory_requirements2,
        "vkGetImageSparseMemoryRequirements" => get_image_sparse_memory_requirements,
        "vkBindImageMemory" => bind_image_memory [big],
        "vkBindImageMemory2" => bind_image_memory2 [big],
        "vkBindImageMemory2KHR" => bind_image_memory2 [big],
        "vkGetImageSubresourceLayout" => get_image_subresource_layout,
        "vkCreateImageView" => create_image_view [big],
        "vkDestroyImageView" => destroy_image_view [big],
        "vkCreateSampler" => create_sampler [big],
        "vkDestroySampler" => destroy_sampler [big],
    ]
}
