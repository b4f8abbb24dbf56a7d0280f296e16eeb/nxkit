//! Textures. Render targets live only in Metal and are copied to guest memory on demand.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLRegion, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage,
};

use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::format::Format;

/// Where the pixels of a texture live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    /// Only the GPU reads and writes it. The default for render targets.
    Private,
    /// The CPU can read and write it too. Apple Silicon shares the memory, so this is cheap.
    Shared,
    /// Tile memory only. The texture has no backing. A pass can use it only with `StoreAction::DontCare`.
    Memoryless,
}

/// What a texture is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureDesc {
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// The pixel format.
    pub format: Format,
    /// The texture is read by shaders.
    pub shader_read: bool,
    /// The texture is written by shaders.
    pub shader_write: bool,
    /// The texture is an attachment of a render pass.
    pub render_target: bool,
    /// Where the pixels live.
    pub storage: Storage,
}

impl TextureDesc {
    /// A 2D texture that shaders read and write, with CPU access (for kernels and tests).
    pub fn compute_2d(width: u32, height: u32, format: Format) -> Self {
        Self {
            width,
            height,
            format,
            shader_read: true,
            shader_write: true,
            render_target: false,
            storage: Storage::Shared,
        }
    }

    /// A 2D render target that shaders can read.
    pub fn render_target_2d(width: u32, height: u32, format: Format) -> Self {
        Self {
            width,
            height,
            format,
            shader_read: true,
            shader_write: false,
            render_target: true,
            storage: Storage::Private,
        }
    }
}

struct Inner {
    raw: Retained<ProtocolObject<dyn MTLTexture>>,
    desc: TextureDesc,
    ctx: Context,
    resident: bool,
}

// SAFETY: `MTLTexture` objects are thread safe reference counted objects and `Inner` never changes them.
unsafe impl Send for Inner {}
// SAFETY: see `Send`.
unsafe impl Sync for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.resident {
            self.ctx.evict(ProtocolObject::from_ref(&*self.raw));
        }
    }
}

/// A 2D texture. Clones share the texture.
#[derive(Clone)]
pub struct Texture {
    inner: Arc<Inner>,
}

impl Texture {
    /// Makes a texture. A texture with backing joins the residency set.
    pub fn new(ctx: &Context, desc: TextureDesc) -> Result<Self> {
        if desc.width == 0 || desc.height == 0 {
            return Err(MetalError::Invalid("a texture needs a width and a height"));
        }
        if desc.storage == Storage::Memoryless && !desc.render_target {
            return Err(MetalError::Invalid(
                "a memoryless texture must be a render target",
            ));
        }
        // SAFETY: width and height are not zero (checked above), which is the one condition of this call.
        let d = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                desc.format.to_mtl(),
                desc.width as usize,
                desc.height as usize,
                false,
            )
        };
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
        d.setUsage(usage);
        d.setStorageMode(match desc.storage {
            Storage::Private => MTLStorageMode::Private,
            Storage::Shared => MTLStorageMode::Shared,
            Storage::Memoryless => MTLStorageMode::Memoryless,
        });
        let raw = ctx
            .device()
            .newTextureWithDescriptor(&d)
            .ok_or_else(|| create("texture", "newTextureWithDescriptor returned nil"))?;
        let resident = desc.storage != Storage::Memoryless;
        if resident {
            ctx.make_resident(ProtocolObject::from_ref(&*raw));
        }
        Ok(Self::wrap(ctx, raw, desc, resident))
    }

    /// Wraps a texture that Metal made, for example the texture of a drawable. It does not join the residency set.
    pub(crate) fn wrap_external(
        ctx: &Context,
        raw: Retained<ProtocolObject<dyn MTLTexture>>,
        desc: TextureDesc,
    ) -> Self {
        Self::wrap(ctx, raw, desc, false)
    }

    fn wrap(
        ctx: &Context,
        raw: Retained<ProtocolObject<dyn MTLTexture>>,
        desc: TextureDesc,
        resident: bool,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                raw,
                desc,
                ctx: ctx.clone(),
                resident,
            }),
        }
    }

    /// The description it was made with.
    pub fn desc(&self) -> TextureDesc {
        self.inner.desc
    }

    pub(crate) fn raw(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.inner.raw
    }

    /// Reads the pixels into `out`, rows `bytes_per_row` apart. Only `Storage::Shared` textures can be read this
    /// way. The GPU must be done with the texture.
    pub fn read(&self, bytes_per_row: usize, out: &mut [u8]) -> Result<()> {
        let d = self.inner.desc;
        if d.storage != Storage::Shared {
            return Err(MetalError::Invalid(
                "only a shared texture can be read by the CPU",
            ));
        }
        let row = d.width as usize * d.format.bytes_per_pixel() as usize;
        if bytes_per_row < row || out.len() < bytes_per_row * d.height as usize {
            return Err(MetalError::Invalid(
                "the output is too small for the texture",
            ));
        }
        let region = MTLRegion {
            origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: d.width as usize,
                height: d.height as usize,
                depth: 1,
            },
        };
        // SAFETY: `out` holds `bytes_per_row * height` bytes (checked above), the region is the whole texture
        // and level 0 exists.
        unsafe {
            self.inner.raw.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::from(&mut *out).cast(),
                bytes_per_row,
                region,
                0,
            );
        }
        Ok(())
    }
}
