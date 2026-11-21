//! Pitch-linear textures over guest memory: the texture reads and writes the bytes of a guest buffer in place.
//!
//! On the console the GPU reads a linear texture from its memory pool, so a CPU write into the pool is a write into
//! the texture. A texture made here over the no-copy buffer of the guest memory behaves the same: no upload, and a
//! byte the CPU wrote is what the next sample reads.

use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLBuffer, MTLDevice, MTLStorageMode, MTLTextureDescriptor, MTLTextureUsage};

use super::{Storage, Texture, TextureDesc, TextureKind};
use crate::device::Context;
use crate::guest::GuestMemory;

/// Why a description cannot be a texture over guest memory. Each is a fact about Metal or the layout, not an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InPlaceRefusal {
    /// Only one 2D image of one level: Metal makes linear textures of `Type2D` only.
    Shape,
    /// Compressed, depth and stencil formats have no linear layout in Metal, and atomics need a texture of their own.
    Format,
    /// The first byte or the row stride is not a multiple of `minimumLinearTextureAlignmentForPixelFormat`.
    Alignment,
    /// The rows are shorter than a row of pixels, or the last row ends after the guest memory.
    Range,
    /// Metal returned no texture.
    Refused,
}

impl Texture {
    /// The alignment Metal needs for the first byte and the row stride of a linear texture of `format` on this device.
    /// `None` for formats without a linear layout.
    pub fn linear_alignment(ctx: &Context, format: crate::Format) -> Option<u64> {
        if format.is_compressed() || format.is_depth() || format.has_stencil() {
            return None;
        }
        let a = ctx
            .device()
            .minimumLinearTextureAlignmentForPixelFormat(format.to_mtl());
        Some((a as u64).max(1))
    }

    /// Checks whether `desc` can be a texture over `len` bytes of guest memory from `offset`, with rows `stride` bytes
    /// apart. `align` is [`Texture::linear_alignment`].
    pub fn in_place_fit(
        desc: &TextureDesc,
        offset: u64,
        stride: u64,
        len: u64,
        align: Option<u64>,
    ) -> std::result::Result<(), InPlaceRefusal> {
        if desc.kind != TextureKind::D2
            || desc.mip_levels != 1
            || desc.array_layers != 1
            || desc.depth != 1
        {
            return Err(InPlaceRefusal::Shape);
        }
        if desc.shader_atomic || desc.storage == Storage::Memoryless {
            return Err(InPlaceRefusal::Format);
        }
        let Some(align) = align else {
            return Err(InPlaceRefusal::Format);
        };
        if !offset.is_multiple_of(align) || !stride.is_multiple_of(align) {
            return Err(InPlaceRefusal::Alignment);
        }
        let row = u64::from(desc.width) * u64::from(desc.format.bytes_per_pixel());
        let end = stride
            .checked_mul(u64::from(desc.height).saturating_sub(1))
            .and_then(|v| v.checked_add(row))
            .and_then(|v| v.checked_add(offset));
        if stride < row || end.is_none_or(|e| e > len) {
            return Err(InPlaceRefusal::Range);
        }
        Ok(())
    }

    /// Makes a 2D texture of one level over the guest memory `mem`: its first texel is at byte `offset`, its rows
    /// `stride` bytes apart. The storage is `Shared` whatever `desc.storage` says. The texture keeps the buffer
    /// alive; it does not join the residency set itself, because its memory is the buffer's, which is in the set.
    pub fn over_guest(
        ctx: &Context,
        mem: &GuestMemory,
        offset: u64,
        stride: u64,
        mut desc: TextureDesc,
    ) -> std::result::Result<Self, InPlaceRefusal> {
        desc.storage = Storage::Shared;
        let align = Self::linear_alignment(ctx, desc.format);
        Self::in_place_fit(&desc, offset, stride, mem.len(), align)?;
        desc.validate().map_err(|_| InPlaceRefusal::Shape)?;
        let d = MTLTextureDescriptor::new();
        d.setTextureType(desc.kind.to_mtl());
        d.setPixelFormat(desc.format.to_mtl());
        // SAFETY: the sizes were checked by `validate`, and the shape is one 2D image of one level.
        unsafe {
            d.setWidth(desc.width as usize);
            d.setHeight(desc.height as usize);
            d.setDepth(1);
            d.setMipmapLevelCount(1);
            d.setArrayLength(1);
        }
        let mut usage = MTLTextureUsage::empty();
        if desc.shader_read {
            usage |= MTLTextureUsage::ShaderRead;
        }
        if desc.shader_write {
            usage |= MTLTextureUsage::ShaderWrite;
        }
        if desc.render_target {
            usage |= MTLTextureUsage::RenderTarget;
        }
        if desc.allow_views {
            usage |= MTLTextureUsage::PixelFormatView;
        }
        d.setUsage(usage);
        d.setStorageMode(MTLStorageMode::Shared);
        let buffer: &ProtocolObject<dyn MTLBuffer> = mem.buffer();
        // `in_place_fit` checked the alignment of the offset and the stride and that every row lies inside the buffer.
        // The texture retains the buffer.
        let raw = buffer
            .newTextureWithDescriptor_offset_bytesPerRow(&d, offset as usize, stride as usize)
            .ok_or(InPlaceRefusal::Refused)?;
        Ok(Self::wrap(ctx, raw, desc, false, None, None, None))
    }
}
