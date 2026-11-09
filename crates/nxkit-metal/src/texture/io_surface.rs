//! A texture over an IOSurface, for memory that Core ML and the Neural Engine share with the GPU.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_io_surface::IOSurfaceRef;
use objc2_metal::{MTLDevice, MTLTextureDescriptor, MTLTextureUsage};

use super::{Texture, TextureDesc};
use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::format::Format;

impl Texture {
    /// Wraps plane 0 of an IOSurface as a 2D texture without a copy. Writes through the texture and writes by another
    /// owner of the surface (Core ML, the CPU) hit the same pages, so the caller orders them, for example with an
    /// `MTLSharedEvent` that the Core ML completion handler signals. The texture keeps the surface alive.
    ///
    /// `format` must have the pixel size of the surface and `width` by `height` must not exceed it. A surface of
    /// `nxkit-coreml` (`SharedTensor`) is `R16Float`, `width` by `height` of the tensor, one channel plane under the
    /// other. `writable` adds shader write usage; a texture that is only read by shaders is `false`.
    pub fn from_io_surface(
        ctx: &Context,
        surface: &IOSurfaceRef,
        width: u32,
        height: u32,
        format: Format,
        writable: bool,
    ) -> Result<Texture> {
        if width == 0 || height == 0 {
            return Err(MetalError::Invalid("an IOSurface texture needs a size"));
        }
        let d = MTLTextureDescriptor::new();
        d.setPixelFormat(format.to_mtl());
        // SAFETY: plain setters on a descriptor that nobody else holds.
        unsafe {
            d.setWidth(width as usize);
            d.setHeight(height as usize);
        }
        d.setUsage(if writable {
            MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite
        } else {
            MTLTextureUsage::ShaderRead
        });
        let raw: Retained<ProtocolObject<dyn objc2_metal::MTLTexture>> = ctx
            .device()
            .newTextureWithDescriptor_iosurface_plane(&d, surface, 0)
            .ok_or_else(|| {
                create(
                    "texture",
                    "newTextureWithDescriptor:iosurface:plane: returned nil",
                )
            })?;
        let mut desc = TextureDesc::compute_2d(width, height, format);
        desc.shader_write = writable;
        ctx.add_allocation(ProtocolObject::from_ref(&*raw));
        let tracked = Some(crate::memstats::Tracked::new(
            crate::memstats::Kind::Texture,
            objc2_metal::MTLResource::allocatedSize(&*raw) as u64,
        ));
        Ok(Self::wrap(ctx, raw, desc, true, None, None, tracked))
    }
}
