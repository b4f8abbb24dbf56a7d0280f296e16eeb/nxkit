//! The descriptions of memory, images, views, buffer views, samplers and descriptor set layouts.

use std::sync::Arc;

use crate::format::{Conversion, PixelFormat};

use super::{MemoryId, SamplerId, Swizzle, TextureKind};

/// The memory that a linear image's texture lies over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LinearBinding {
    /// The memory.
    pub memory: MemoryId,
    /// The offset of the first texel in the memory.
    pub offset: u64,
    /// The bytes from one row to the next (Metal's linear texture alignment).
    pub row_pitch: u64,
}

/// An image as the backend makes it.
///
/// An image has a texture of its own and ignores its memory binding, except a linear 2D image with one level, one layer
/// and one sample, which aliases its memory as on the Switch: its texture lies over the bound memory ([`Self::linear`]),
/// so the backend makes it at `vkBindImageMemory`. A linear image of a converted format ([`Conversion`]) gets a texture
/// of its own, which the backend fills from the memory before each submit that follows a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageDesc {
    /// The `VkImageType` (0 = 1D, 1 = 2D, 2 = 3D).
    pub image_type: u32,
    /// The `VkFormat`.
    pub format: u32,
    /// The pixel format of the texture.
    pub pixel: PixelFormat,
    /// The type of the texture.
    pub kind: TextureKind,
    /// Width, height and depth; at least 1 each.
    pub extent: [u32; 3],
    /// The mip levels.
    pub levels: u32,
    /// The array layers (6 per cube).
    pub layers: u32,
    /// The samples per texel.
    pub samples: u32,
    /// The `VkImageUsageFlags`.
    pub usage: u32,
    /// The `VkImageCreateFlags`.
    pub flags: u32,
    /// Whether the texture must allow views of other pixel formats (mutable format, or a depth-stencil image whose
    /// stencil a view may take).
    pub format_views: bool,
    /// The texture over the bound memory of a linear image, None for an image with a texture of its own.
    pub linear: Option<LinearBinding>,
    /// The unpacking that a converted format needs.
    pub conversion: Conversion,
    /// Whether the image is one of a swapchain's.
    pub swapchain: bool,
}

/// An image view as the backend makes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewDesc {
    /// The `VkFormat` of the view.
    pub format: u32,
    /// The pixel format of the view (`X32Stencil8` for the stencil of a depth-stencil image).
    pub pixel: PixelFormat,
    /// The type of the view.
    pub kind: TextureKind,
    /// The first level.
    pub base_level: u32,
    /// The levels.
    pub levels: u32,
    /// The first layer.
    pub base_layer: u32,
    /// The layers.
    pub layers: u32,
    /// The swizzle (alpha is one for the RGB formats of BC1).
    pub swizzle: [Swizzle; 4],
    /// The `VkImageAspectFlags` of the range.
    pub aspect: u32,
    /// Whether the view covers the image as it is: same format, type, levels, layers, identity swizzle. The backend may
    /// use the image's texture itself.
    pub whole: bool,
}

/// A texel buffer view: a linear texture over a buffer's memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferViewDesc {
    /// The memory of the buffer.
    pub memory: MemoryId,
    /// The first byte, from the start of the memory.
    pub offset: u64,
    /// The `VkFormat`.
    pub format: u32,
    /// The pixel format.
    pub pixel: PixelFormat,
    /// The texels.
    pub elements: u64,
}

/// A sampler (`VkSamplerCreateInfo`, normalized).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SamplerDesc {
    /// `magFilter` (`VkFilter`: 0 nearest, 1 linear).
    pub mag_filter: u32,
    /// `minFilter`.
    pub min_filter: u32,
    /// `mipmapMode` (0 nearest, 1 linear).
    pub mipmap_mode: u32,
    /// `addressModeU`, `V`, `W` (`VkSamplerAddressMode`).
    pub address: [u32; 3],
    /// `mipLodBias`, clamped to the device's 15.
    pub lod_bias: f32,
    /// The anisotropy: 1 when it is off, else `maxAnisotropy` clamped to 1..16.
    pub max_anisotropy: f32,
    /// The compare op (`VkCompareOp`) when comparison is on.
    pub compare: Option<u32>,
    /// `minLod`.
    pub min_lod: f32,
    /// `maxLod`.
    pub max_lod: f32,
    /// `borderColor` (`VkBorderColor`).
    pub border_color: u32,
    /// `unnormalizedCoordinates`.
    pub unnormalized: bool,
}

/// One binding of a descriptor set layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutBinding {
    /// The binding number.
    pub binding: u32,
    /// The `VkDescriptorType`.
    pub ty: u32,
    /// The elements of the binding.
    pub count: u32,
    /// The `VkShaderStageFlags`.
    pub stages: u32,
    /// The index of its first element in the set's flat array.
    pub base: u32,
    /// The index of its first dynamic offset in the set, for a dynamic buffer type.
    pub dynamic: Option<u32>,
    /// The immutable samplers, one per element, or empty.
    pub immutable: Vec<SamplerId>,
}

impl LayoutBinding {
    /// Whether the binding is a dynamic uniform or storage buffer.
    pub fn is_dynamic(&self) -> bool {
        self.dynamic.is_some()
    }
}

/// A descriptor set layout: its bindings sorted by number, which is the order that dynamic offsets are taken in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetLayoutDesc {
    /// The bindings, sorted by binding number.
    pub bindings: Vec<LayoutBinding>,
    /// The elements of all bindings.
    pub total: u32,
    /// The dynamic offsets that a bind of the set takes.
    pub dynamic_count: u32,
}

impl SetLayoutDesc {
    /// The position in [`Self::bindings`] of a binding number.
    pub fn index_of(&self, binding: u32) -> Option<usize> {
        self.bindings
            .binary_search_by_key(&binding, |b| b.binding)
            .ok()
    }

    /// The flat index of element `elem` of `binding`, and the binding that holds it. An element past the binding's
    /// count goes on into the next bindings (Vulkan's consecutive-binding updates).
    pub fn element(&self, binding: u32, elem: u32) -> Option<(u32, &LayoutBinding)> {
        let mut i = self.index_of(binding)?;
        let mut e = elem;
        loop {
            let b = self.bindings.get(i)?;
            if e < b.count {
                return Some((b.base + e, b));
            }
            e -= b.count;
            i += 1;
        }
    }
}

/// A push constant range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PushRange {
    /// The `VkShaderStageFlags`.
    pub stages: u32,
    /// The first byte.
    pub offset: u32,
    /// The bytes.
    pub size: u32,
}

/// A pipeline layout: its set layouts (held, so a set layout that the game destroyed lives on in the pipeline) and its
/// push constant ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PipelineLayoutDesc {
    /// The set layouts, at most 8.
    pub sets: Vec<Arc<SetLayoutDesc>>,
    /// The push constant ranges.
    pub push_ranges: Vec<PushRange>,
    /// The end of the highest push constant range, at most 256.
    pub push_size: u32,
}

/// `VkImageSubresourceRange`. `REMAINING` counts stay as `u32::MAX` until the front end resolves them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SubresourceRange {
    /// `aspectMask`.
    pub aspect: u32,
    /// `baseMipLevel`.
    pub base_level: u32,
    /// `levelCount`.
    pub levels: u32,
    /// `baseArrayLayer`.
    pub base_layer: u32,
    /// `layerCount`.
    pub layers: u32,
}

impl SubresourceRange {
    /// The range with `REMAINING` counts resolved against an image of `levels` levels and `layers` layers.
    pub fn resolve(self, levels: u32, layers: u32) -> Self {
        let rem = crate::consts::special::REMAINING;
        Self {
            levels: if self.levels == rem {
                levels.saturating_sub(self.base_level)
            } else {
                self.levels
            },
            layers: if self.layers == rem {
                layers.saturating_sub(self.base_layer)
            } else {
                self.layers
            },
            ..self
        }
    }
}
