//! Texel buffer views on a real Metal 4 GPU: a view over the storage of a `VkDeviceMemory` is made as a texture buffer,
//! and a view that Metal cannot make is refused with a reason. Without a device it prints a message and passes.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nxkit_metal::Context;
use nxkit_vk::Backend;
use nxkit_vk::desc::{BufferViewDesc, MemoryId, TextureId};
use nxkit_vk::format::PixelFormat;
use nxkit_vk_gpu::{ShaderTable, VkGpu, VkGpuSettings};

fn view(memory: MemoryId, offset: u64, elements: u64) -> BufferViewDesc {
    BufferViewDesc {
        memory,
        offset,
        // VK_FORMAT_R32_UINT, the texel buffer Hades makes
        format: 98,
        pixel: PixelFormat::R32Uint,
        elements,
    }
}

#[test]
fn a_texel_buffer_view_lies_over_the_memory() {
    let Ok(ctx) = Context::system_default() else {
        eprintln!("skipped: no usable Metal 4 device");
        return;
    };
    let gpu = VkGpu::with_shaders(
        &ctx,
        ShaderTable::new(None, Vec::new()),
        VkGpuSettings::default(),
    )
    .unwrap();
    let mem = MemoryId(1);
    gpu.create_memory(mem, 64 * 1024, 1).unwrap();
    // the whole memory, and a part of it from an aligned offset
    gpu.create_buffer_view(TextureId(10), &view(mem, 0, 16 * 1024))
        .unwrap();
    gpu.create_buffer_view(TextureId(11), &view(mem, 4096, 256))
        .unwrap();
    gpu.destroy_texture(TextureId(10));
    gpu.destroy_texture(TextureId(11));
    // texels past the end, an unaligned offset and memory without storage are refused with a reason
    let past = gpu.create_buffer_view(TextureId(12), &view(mem, 0, 16 * 1024 + 1));
    assert!(past.is_err_and(|e| e.0.contains("Range")));
    let odd = gpu.create_buffer_view(TextureId(13), &view(mem, 3, 4));
    assert!(odd.is_err_and(|e| e.0.contains("Alignment")));
    let none = gpu.create_buffer_view(TextureId(14), &view(MemoryId(2), 0, 4));
    assert!(none.is_err_and(|e| e.0.contains("no storage")));
}
