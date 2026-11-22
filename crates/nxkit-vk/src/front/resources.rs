//! Memory, buffers, buffer views, images, image views and samplers.

use crate::consts::{image_flags, image_type, special::WHOLE_SIZE, tiling, view_type};
use crate::desc::{
    BufferViewDesc, IDENTITY_SWIZZLE, ImageDesc, LinearBinding, MemoryId, SamplerDesc, SamplerId,
    Swizzle, TextureId, TextureKind, ViewDesc,
};
use crate::device::{self, ALL_MEMORY_TYPES, BUFFER_ALIGNMENT, IMAGE_ALIGNMENT};
use crate::format::{self, Class, PixelFormat, aspect};
use crate::info::{BufferInfo, BufferViewInfo, ImageInfo, ImageViewInfo};
use crate::result::{VkError, VkResult};
use crate::write::{Requirements, SubresourceLayout};

use super::Vk;
use super::state::{Buffer, Image, ImageView, Memory};

/// The texture type of an image (the reference layer's rules: 1D is a one-row 2D, cube only with a multiple of 6).
pub(crate) fn image_kind(info: &ImageInfo, layers: u32, samples: u32) -> TextureKind {
    let cube = info.flags & image_flags::CUBE_COMPATIBLE != 0 && layers.is_multiple_of(6);
    if info.image_type == image_type::D3 {
        TextureKind::D3
    } else if samples > 1 {
        if layers > 1 {
            TextureKind::D2MultisampleArray
        } else {
            TextureKind::D2Multisample
        }
    } else if cube {
        if layers > 6 {
            TextureKind::CubeArray
        } else {
            TextureKind::Cube
        }
    } else if layers > 1 {
        TextureKind::D2Array
    } else {
        TextureKind::D2
    }
}

impl Vk {
    /// `vkAllocateMemory`. A memory type above 2 gives `VK_ERROR_OUT_OF_DEVICE_MEMORY`. The storage is made later.
    pub fn allocate_memory(&self, size: u64, type_index: u32) -> VkResult<u64> {
        if type_index as usize >= device::MEMORY_TYPES.len() {
            return Err(VkError::OutOfDeviceMemory);
        }
        Ok(self.lock().memories.insert(Memory {
            size,
            type_index,
            host: None,
        }))
    }

    /// `vkFreeMemory`.
    pub fn free_memory(&self, memory: u64) {
        let m = self.lock().memories.remove(memory);
        if m.is_some_and(|m| m.host.is_some()) {
            self.backend.destroy_memory(MemoryId(memory));
        }
    }

    /// The storage of a memory, made on first use. Gives its host address. The storage is made with the front lock
    /// held, so two threads that bind the same memory at once get one storage (it is one buffer allocation).
    fn storage(&self, memory: u64) -> VkResult<u64> {
        let mut s = self.lock();
        let m = s
            .memories
            .get_mut(memory)
            .ok_or_else(|| self.bad("memory", memory))?;
        if let Some(h) = m.host {
            return Ok(h);
        }
        let size = m.size;
        let host = self
            .backend
            .create_memory(MemoryId(memory), size, m.type_index)
            .map_err(|e| {
                self.log.once(
                    "memory storage",
                    format_args!("[RED] cannot make {size} bytes of memory: {e}"),
                );
                VkError::OutOfDeviceMemory
            })?;
        m.host = Some(host);
        Ok(host)
    }

    /// `vkMapMemory`: the host address of byte `offset`. Device-local-only memory (type 0) is mapped too, and reported.
    pub fn map_memory(&self, memory: u64, offset: u64) -> VkResult<u64> {
        let ty = self.lock().memories.get(memory).map(|m| m.type_index);
        if ty == Some(0) {
            self.log
                .once("map type 0", "[RED] mapped invisible memory (type 0)");
        }
        let host = self.storage(memory).map_err(|e| match e {
            VkError::OutOfDeviceMemory => VkError::MemoryMapFailed,
            e => e,
        })?;
        Ok(host + offset)
    }

    /// `vkCreateBuffer`.
    pub fn create_buffer(&self, info: &BufferInfo) -> u64 {
        self.lock().buffers.insert(Buffer {
            info: *info,
            memory: 0,
            offset: 0,
        })
    }

    /// `vkDestroyBuffer`.
    pub fn destroy_buffer(&self, buffer: u64) {
        self.lock().buffers.remove(buffer);
    }

    /// `vkGetBufferMemoryRequirements`: alignment 256 (Metal's constant buffer offset alignment), every memory type.
    pub fn buffer_requirements(&self, buffer: u64) -> VkResult<Requirements> {
        let s = self.lock();
        let b = s
            .buffers
            .get(buffer)
            .ok_or_else(|| self.bad("buffer", buffer))?;
        Ok(Requirements {
            size: b.info.size.next_multiple_of(BUFFER_ALIGNMENT),
            alignment: BUFFER_ALIGNMENT,
            type_bits: ALL_MEMORY_TYPES,
        })
    }

    /// `vkBindBufferMemory`: the buffer lies at `offset` of the memory, whose storage is made now.
    pub fn bind_buffer_memory(&self, buffer: u64, memory: u64, offset: u64) -> VkResult<()> {
        {
            let mut s = self.lock();
            let b = s
                .buffers
                .get_mut(buffer)
                .ok_or_else(|| self.bad("buffer", buffer))?;
            b.memory = memory;
            b.offset = offset;
        }
        self.storage(memory).map(|_| ())
    }

    /// `vkCreateBufferView`. A format that no texture can have is reported, and the view binds nothing.
    pub fn create_buffer_view(&self, info: &BufferViewInfo) -> VkResult<u64> {
        let (h, b) = {
            let mut s = self.lock();
            let b = *s
                .buffers
                .get(info.buffer)
                .ok_or_else(|| self.bad("buffer", info.buffer))?;
            (s.buffer_views.insert(info.buffer), b)
        };
        let Some(f) = format::texture_format(info.format) else {
            self.log.once(
                "buffer view format",
                format_args!("[RED] buffer view format {} not mapped", info.format),
            );
            return Ok(h);
        };
        if b.memory == 0 {
            self.log.once(
                "buffer view unbound",
                "[RED] a buffer view of a buffer without memory binds nothing",
            );
            return Ok(h);
        }
        let range = if info.range == WHOLE_SIZE {
            b.info.size.saturating_sub(info.offset)
        } else {
            info.range
        };
        let desc = BufferViewDesc {
            memory: MemoryId(b.memory),
            offset: b.offset + info.offset,
            format: info.format,
            pixel: f.pixel.unwrap_or(PixelFormat::R8Unorm),
            elements: range / u64::from(f.bytes.max(1)),
        };
        if let Err(e) = self.backend.create_buffer_view(TextureId(h), &desc) {
            self.log.once(
                "buffer view",
                format_args!("[RED] cannot make a texel buffer: {e}"),
            );
        }
        Ok(h)
    }

    /// `vkDestroyBufferView`.
    pub fn destroy_buffer_view(&self, view: u64) {
        if self.lock().buffer_views.remove(view).is_some() {
            self.backend.destroy_texture(TextureId(view));
        }
    }

    /// `vkCreateImage`.
    ///
    /// A linear 2D image with one level, one layer and one sample aliases its memory: its texture is made over the
    /// memory at `vkBindImageMemory`, with rows at Metal's linear texture alignment. Every other image gets a texture
    /// of its own now and ignores its memory.
    pub fn create_image(&self, info: &ImageInfo) -> VkResult<u64> {
        let Some(f) = format::texture_format(info.format) else {
            let e = VkError::FormatNotSupported;
            return Err(self.log.unsupported(
                "image format",
                e,
                format_args!("image format {}", info.format),
            ));
        };
        let pixel = f.pixel.unwrap_or(PixelFormat::R8Unorm);
        let layers = info.layers.max(1);
        let samples = info.samples.max(1);
        let extent = [
            info.extent[0].max(1),
            info.extent[1].max(1),
            info.extent[2].max(1),
        ];
        let linear = info.tiling == tiling::LINEAR
            && info.image_type == image_type::D2
            && info.levels <= 1
            && layers == 1
            && samples == 1
            && f.class != Class::Block
            && !f.has_depth()
            && !f.has_stencil();
        if f.needs_conversion() && !linear {
            let e = VkError::FormatNotSupported;
            return Err(self.log.unsupported(
                "converted optimal",
                e,
                format_args!("format {} is converted only for linear images", info.format),
            ));
        }
        if info.tiling == tiling::LINEAR && !linear {
            self.log.once("linear image", format_args!("[RED] linear image {:?} format {} levels {} layers {}: its content does not follow the bound memory", info.extent, info.format, info.levels, layers));
        }
        let mut desc = ImageDesc {
            image_type: info.image_type,
            format: info.format,
            pixel,
            kind: image_kind(info, layers, samples),
            extent,
            levels: info.levels.max(1),
            layers,
            samples,
            usage: info.usage,
            flags: info.flags,
            format_views: info.flags & image_flags::MUTABLE_FORMAT != 0
                || pixel == PixelFormat::Depth32FloatStencil8,
            linear: None,
            conversion: f.conversion,
            swapchain: false,
        };
        let (pitch, size) = if linear {
            desc.levels = 1;
            let al = self.backend.linear_alignment(pixel).max(1);
            let pitch = (u64::from(extent[0]) * u64::from(f.bytes)).next_multiple_of(al);
            (pitch, pitch * u64::from(extent[1]))
        } else {
            (
                0,
                f.image_bytes(
                    extent[0],
                    extent[1],
                    extent[2],
                    desc.levels,
                    layers,
                    samples,
                ),
            )
        };
        let image = Image {
            desc: desc.clone(),
            pitch,
            size,
            created: !linear,
        };
        let h = self.lock().images.insert(image);
        if !linear && let Err(e) = self.backend.create_image(TextureId(h), &desc) {
            self.log.once(
                "image",
                format_args!(
                    "[RED] cannot make image {extent:?} format {}: {e}",
                    info.format
                ),
            );
            self.lock().images.remove(h);
            return Err(VkError::OutOfDeviceMemory);
        }
        Ok(h)
    }

    /// `vkDestroyImage`. A swapchain image is the swapchain's: a destroy of it is ignored (Super Mario 3D All-Stars
    /// destroys them as it tears down and keeps the swapchain).
    pub fn destroy_image(&self, image: u64) {
        let created = {
            let mut s = self.lock();
            match s.images.get(image) {
                Some(i) if i.desc.swapchain => return,
                Some(_) => s.images.remove(image).is_some_and(|i| i.created),
                None => return,
            }
        };
        if created {
            self.backend.destroy_texture(TextureId(image));
        }
    }

    /// `vkGetImageMemoryRequirements`: alignment 4096, every memory type.
    pub fn image_requirements(&self, image: u64) -> VkResult<Requirements> {
        let s = self.lock();
        let i = s
            .images
            .get(image)
            .ok_or_else(|| self.bad("image", image))?;
        Ok(Requirements {
            size: i.size.next_multiple_of(IMAGE_ALIGNMENT),
            alignment: IMAGE_ALIGNMENT,
            type_bits: ALL_MEMORY_TYPES,
        })
    }

    /// `vkBindImageMemory`. Only a linear image uses its memory: its texture is made over it now (or, for a converted
    /// format, a texture of its own that the backend fills from the memory).
    pub fn bind_image_memory(&self, image: u64, memory: u64, offset: u64) -> VkResult<()> {
        let pitch = {
            let s = self.lock();
            s.images
                .get(image)
                .ok_or_else(|| self.bad("image", image))?
                .pitch
        };
        if pitch == 0 {
            return Ok(());
        }
        self.storage(memory)?;
        let desc = {
            let mut s = self.lock();
            let i = s
                .images
                .get_mut(image)
                .ok_or_else(|| self.bad("image", image))?;
            if i.created {
                self.log.once(
                    "rebind image",
                    "[RED] a linear image was bound twice; the first binding stays",
                );
                return Ok(());
            }
            i.desc.linear = Some(LinearBinding {
                memory: MemoryId(memory),
                offset,
                row_pitch: pitch,
            });
            i.created = true;
            i.desc.clone()
        };
        self.backend
            .create_image(TextureId(image), &desc)
            .map_err(|e| {
                self.log.once(
                    "linear image",
                    format_args!("[RED] cannot make linear image {:?}: {e}", desc.extent),
                );
                if let Some(i) = self.lock().images.get_mut(image) {
                    i.created = false;
                }
                VkError::OutOfDeviceMemory
            })
    }

    /// `vkGetImageSubresourceLayout`: a linear image's own pitch, and other images tightly packed, levels then layers.
    pub fn subresource_layout(
        &self,
        image: u64,
        level: u32,
        layer: u32,
    ) -> VkResult<SubresourceLayout> {
        let s = self.lock();
        let i = s
            .images
            .get(image)
            .ok_or_else(|| self.bad("image", image))?;
        if i.pitch != 0 {
            return Ok(SubresourceLayout {
                offset: 0,
                size: i.size,
                row_pitch: i.pitch,
                array_pitch: i.size,
                depth_pitch: i.size,
            });
        }
        let Some(f) = format::lookup(i.desc.format) else {
            return Ok(SubresourceLayout::default());
        };
        let d = &i.desc;
        let mip = |m: u32| {
            [
                (d.extent[0] >> m).max(1),
                (d.extent[1] >> m).max(1),
                (d.extent[2] >> m).max(1),
            ]
        };
        let before: u64 = (0..level.min(d.levels))
            .map(|m| {
                let e = mip(m);
                f.level_bytes(e[0], e[1], e[2]) * u64::from(d.layers) * u64::from(d.samples)
            })
            .sum();
        let [w, h, depth] = mip(level);
        let row = u64::from(w.div_ceil(u32::from(f.block_w.max(1)))) * u64::from(f.bytes);
        let depth_pitch = row * u64::from(h.div_ceil(u32::from(f.block_h.max(1))));
        let array_pitch = depth_pitch * u64::from(depth);
        Ok(SubresourceLayout {
            offset: before + array_pitch * u64::from(layer),
            size: array_pitch,
            row_pitch: row,
            array_pitch,
            depth_pitch,
        })
    }

    /// `vkCreateImageView`.
    pub fn create_image_view(&self, info: &ImageViewInfo) -> VkResult<u64> {
        let image = self
            .lock()
            .images
            .get(info.image)
            .cloned()
            .ok_or_else(|| self.bad("image", info.image))?;
        let d = &image.desc;
        let mut pixel = format::texture_format(info.format)
            .and_then(|f| f.pixel)
            .unwrap_or(d.pixel);
        let r = info.range.resolve(d.levels, d.layers);
        if r.aspect == aspect::STENCIL && pixel == PixelFormat::Depth32FloatStencil8 {
            pixel = PixelFormat::X32Stencil8;
        }
        let multi = d.samples > 1;
        let kind = match info.view_type {
            view_type::D1_ARRAY => TextureKind::D2Array,
            view_type::D3 => TextureKind::D3,
            view_type::CUBE => TextureKind::Cube,
            view_type::CUBE_ARRAY => TextureKind::CubeArray,
            view_type::D2_ARRAY if multi => TextureKind::D2MultisampleArray,
            view_type::D2_ARRAY => TextureKind::D2Array,
            _ if multi => TextureKind::D2Multisample,
            _ => TextureKind::D2,
        };
        let c = info.components;
        let alpha = if format::is_bc1_rgb(d.format) {
            Swizzle::One
        } else {
            Swizzle::from_vk(c[3], Swizzle::A)
        };
        let swizzle = [
            Swizzle::from_vk(c[0], Swizzle::R),
            Swizzle::from_vk(c[1], Swizzle::G),
            Swizzle::from_vk(c[2], Swizzle::B),
            alpha,
        ];
        let whole = pixel == d.pixel
            && kind == d.kind
            && r.base_level == 0
            && r.levels == d.levels
            && r.base_layer == 0
            && r.layers == d.layers
            && swizzle == IDENTITY_SWIZZLE;
        let desc = ViewDesc {
            format: if format::texture_format(info.format).is_some() {
                info.format
            } else {
                d.format
            },
            pixel,
            kind,
            base_level: r.base_level,
            levels: r.levels,
            base_layer: if kind == TextureKind::D3 {
                0
            } else {
                r.base_layer
            },
            layers: if kind == TextureKind::D3 { 1 } else { r.layers },
            swizzle,
            aspect: r.aspect,
            whole,
        };
        let h = self.lock().views.insert(ImageView {
            image: info.image,
            desc,
            samples: d.samples,
        });
        if let Err(e) = self
            .backend
            .create_view(TextureId(h), TextureId(info.image), &desc)
        {
            self.log.once(
                "image view",
                format_args!(
                    "[RED] cannot make image view of format {}: {e}",
                    desc.format
                ),
            );
        }
        Ok(h)
    }

    /// `vkDestroyImageView`.
    pub fn destroy_image_view(&self, view: u64) {
        if self.lock().views.remove(view).is_some() {
            self.backend.destroy_texture(TextureId(view));
        }
    }

    /// `vkCreateSampler`.
    pub fn create_sampler(&self, desc: &SamplerDesc) -> VkResult<u64> {
        let h = self.lock().samplers.insert(*desc);
        if let Err(e) = self.backend.create_sampler(SamplerId(h), desc) {
            self.log.once(
                "sampler",
                format_args!("[RED] cannot make the sampler: {e}"),
            );
        }
        Ok(h)
    }

    /// `vkDestroySampler`.
    pub fn destroy_sampler(&self, sampler: u64) {
        if self.lock().samplers.remove(sampler).is_some() {
            self.backend.destroy_sampler(SamplerId(sampler));
        }
    }
}
