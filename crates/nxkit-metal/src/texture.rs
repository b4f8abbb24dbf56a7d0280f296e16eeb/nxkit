//! Textures. Render targets live only in Metal and are copied to guest memory on demand.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTLDevice, MTLOrigin, MTLRegion, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureSwizzle, MTLTextureSwizzleChannels, MTLTextureType, MTLTextureUsage,
};

use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::format::Format;

/// Where the pixels of a texture live.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Storage {
    /// Only the GPU reads and writes it. The default for render targets.
    Private,
    /// The CPU can read and write it too. Apple Silicon shares the memory, so this is cheap.
    Shared,
    /// Tile memory only. The texture has no backing. A pass can use it only with `StoreAction::DontCare`.
    Memoryless,
}

/// The shape of a texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureKind {
    /// One 2D image per level.
    D2,
    /// An array of 2D images.
    D2Array,
    /// A volume. The depth is `TextureDesc::depth`.
    D3,
    /// Six faces, which count as 6 array layers (face `i` is layer `i`).
    Cube,
    /// An array of cubes. `array_layers` counts cubes and the faces are layers `6 * cube + face`.
    CubeArray,
}

impl TextureKind {
    fn to_mtl(self) -> MTLTextureType {
        match self {
            Self::D2 => MTLTextureType::Type2D,
            Self::D2Array => MTLTextureType::Type2DArray,
            Self::D3 => MTLTextureType::Type3D,
            Self::Cube => MTLTextureType::TypeCube,
            Self::CubeArray => MTLTextureType::TypeCubeArray,
        }
    }
}

/// What a texture is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureDesc {
    /// The shape.
    pub kind: TextureKind,
    /// The width of level 0 in pixels.
    pub width: u32,
    /// The height of level 0 in pixels.
    pub height: u32,
    /// The depth of level 0 of a 3D texture. It is 1 for the other kinds.
    pub depth: u32,
    /// The number of mip levels, at least 1.
    pub mip_levels: u32,
    /// The array length: layers of `D2Array` or cubes of `CubeArray`. It is 1 for the other kinds.
    pub array_layers: u32,
    /// Views of another pixel format may be made, for example the unsigned integer views of the block-linear kernels.
    pub allow_views: bool,
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
            kind: TextureKind::D2,
            width,
            height,
            depth: 1,
            mip_levels: 1,
            array_layers: 1,
            allow_views: false,
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
            kind: TextureKind::D2,
            width,
            height,
            depth: 1,
            mip_levels: 1,
            array_layers: 1,
            allow_views: false,
            format,
            shader_read: true,
            shader_write: false,
            render_target: true,
            storage: Storage::Private,
        }
    }
}

impl TextureDesc {
    /// The size of a level in pixels. The depth is 1 unless the texture is 3D.
    pub fn level_extent(&self, level: u32) -> [u32; 3] {
        let m = |v: u32| v.checked_shr(level).unwrap_or(0).max(1);
        [
            m(self.width),
            m(self.height),
            if self.kind == TextureKind::D3 {
                m(self.depth)
            } else {
                1
            },
        ]
    }

    /// The number of 2D slices a texture has: the layers, with 6 per cube.
    pub fn slice_count(&self) -> u32 {
        match self.kind {
            TextureKind::D2 | TextureKind::D3 => 1,
            TextureKind::D2Array => self.array_layers,
            TextureKind::Cube => 6,
            TextureKind::CubeArray => self.array_layers * 6,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.width == 0
            || self.height == 0
            || self.depth == 0
            || self.mip_levels == 0
            || self.array_layers == 0
        {
            return Err(MetalError::Invalid(
                "a texture needs a size, a level and a layer",
            ));
        }
        if self.mip_levels > 32 - self.width.max(self.height).max(self.depth).leading_zeros() {
            return Err(MetalError::Invalid(
                "the texture has more mip levels than its size allows",
            ));
        }
        let cube = matches!(self.kind, TextureKind::Cube | TextureKind::CubeArray);
        if cube && self.width != self.height {
            return Err(MetalError::Invalid("a cube texture must be square"));
        }
        if self.kind != TextureKind::D3 && self.depth != 1 {
            return Err(MetalError::Invalid("only a 3D texture has a depth"));
        }
        if matches!(
            self.kind,
            TextureKind::D2 | TextureKind::D3 | TextureKind::Cube
        ) && self.array_layers != 1
        {
            return Err(MetalError::Invalid("this texture kind has one array layer"));
        }
        Ok(())
    }
}

/// A view of a texture: another format or kind over a range of its levels and slices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewDesc {
    /// The format of the view. It must have the pixel size of the texture's format.
    pub format: Format,
    /// The kind of the view.
    pub kind: TextureKind,
    /// The first level.
    pub first_level: u32,
    /// The number of levels.
    pub level_count: u32,
    /// The first slice.
    pub first_slice: u32,
    /// The number of slices (cubes for `CubeArray`).
    pub slice_count: u32,
}

struct Inner {
    /// The texture a view was made from. It stays alive as long as the view.
    _parent: Option<Texture>,
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
            self.ctx
                .remove_allocation(ProtocolObject::from_ref(&*self.raw));
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
        desc.validate()?;
        if desc.storage == Storage::Memoryless && !desc.render_target {
            return Err(MetalError::Invalid(
                "a memoryless texture must be a render target",
            ));
        }
        let d = MTLTextureDescriptor::new();
        d.setTextureType(desc.kind.to_mtl());
        d.setPixelFormat(desc.format.to_mtl());
        // SAFETY: the sizes, levels and layers were checked by `validate`.
        unsafe {
            d.setWidth(desc.width as usize);
            d.setHeight(desc.height as usize);
            d.setDepth(desc.depth as usize);
            d.setMipmapLevelCount(desc.mip_levels as usize);
            d.setArrayLength(desc.array_layers as usize);
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
            ctx.add_allocation(ProtocolObject::from_ref(&*raw));
        }
        Ok(Self::wrap(ctx, raw, desc, resident, None))
    }

    /// Wraps a texture that Metal made, for example the texture of a drawable. It does not join the residency set.
    pub(crate) fn wrap_external(
        ctx: &Context,
        raw: Retained<ProtocolObject<dyn MTLTexture>>,
        desc: TextureDesc,
    ) -> Self {
        Self::wrap(ctx, raw, desc, false, None)
    }

    fn wrap(
        ctx: &Context,
        raw: Retained<ProtocolObject<dyn MTLTexture>>,
        desc: TextureDesc,
        resident: bool,
        parent: Option<Texture>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                _parent: parent,
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

    /// Reads single pixels of level 0, slice 0 back to the CPU. Diagnostics only. It makes its own command queue and
    /// waits, so it sees what the GPU wrote before the call only when the caller waited for that work. Returns
    /// `None` for compressed formats and formats with a stencil aspect.
    pub fn debug_read_raw(&self, ctx: &Context, points: &[[u32; 2]]) -> Option<Vec<Vec<u8>>> {
        use objc2_metal::{
            MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
            MTLResourceOptions,
        };
        let f = self.inner.desc.format;
        let bpp = f.bytes_per_pixel() as usize;
        if f.is_compressed() || f.has_stencil() || bpp == 0 || bpp > 16 || points.is_empty() {
            return None;
        }
        let dev = ctx.device();
        let queue = dev.newCommandQueue()?;
        let buf = dev.newBufferWithLength_options(
            bpp * points.len(),
            MTLResourceOptions::StorageModeShared,
        )?;
        let cb = queue.commandBuffer()?;
        let enc = cb.blitCommandEncoder()?;
        for (i, p) in points.iter().enumerate() {
            let x = p[0].min(self.inner.desc.width.saturating_sub(1));
            let y = p[1].min(self.inner.desc.height.saturating_sub(1));
            // SAFETY: the 1x1 source box is inside level 0 (clamped above) and the destination is `bpp` bytes
            // inside `buf`.
            unsafe {
                enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &self.inner.raw,
                    0,
                    0,
                    MTLOrigin { x: x as usize, y: y as usize, z: 0 },
                    MTLSize { width: 1, height: 1, depth: 1 },
                    &buf,
                    bpp * i,
                    bpp,
                    bpp,
                );
            }
        }
        enc.endEncoding();
        cb.commit();
        cb.waitUntilCompleted();
        // SAFETY: the GPU is done with `buf` (waited above) and it holds `bpp * points.len()` shared bytes.
        let bytes =
            unsafe { std::slice::from_raw_parts(buf.contents().cast::<u8>(), bpp * points.len()) };
        Some(bytes.chunks_exact(bpp).map(<[u8]>::to_vec).collect())
    }

    /// [`Texture::debug_read_raw`] for formats of 4 bytes per pixel.
    pub fn debug_read(&self, ctx: &Context, points: &[[u32; 2]]) -> Option<Vec<[u8; 4]>> {
        if self.inner.desc.format.bytes_per_pixel() != 4 {
            return None;
        }
        let raw = self.debug_read_raw(ctx, points)?;
        Some(raw.iter().map(|c| [c[0], c[1], c[2], c[3]]).collect())
    }

    /// The 64 bit resource ID that a shader reads from an argument buffer to sample or write this texture.
    /// The ID stays valid as long as the texture lives.
    pub fn gpu_resource_id(&self) -> u64 {
        self.inner.raw.gpuResourceID().to_raw()
    }

    /// Makes a view of this texture. The view keeps the texture alive and shares its residency.
    pub fn view(&self, v: &ViewDesc) -> Result<Texture> {
        self.view_swizzled(v, [2, 3, 4, 5])
    }

    /// Makes a view with channel selectors: 0 zero, 1 one, 2 red, 3 green, 4 blue and 5 alpha.
    pub fn view_swizzled(&self, v: &ViewDesc, channels: [u8; 4]) -> Result<Texture> {
        if channels.iter().any(|c| *c > 5) {
            return Err(MetalError::Invalid("invalid texture channel selector"));
        }
        let d = self.inner.desc;
        if !d.allow_views && v.format != d.format {
            return Err(MetalError::Invalid(
                "the texture was not made with `allow_views`",
            ));
        }
        // Metal aborts the process on a view between a block compressed and an uncompressed format.
        if v.format.is_compressed() != d.format.is_compressed() {
            return Err(MetalError::Invalid(
                "a view cannot change between a compressed and an uncompressed format",
            ));
        }
        if v.format.bytes_per_pixel() != d.format.bytes_per_pixel() {
            return Err(MetalError::Invalid(
                "a view needs the pixel size of the texture",
            ));
        }
        if v.level_count == 0
            || v.slice_count == 0
            || v.first_level.saturating_add(v.level_count) > d.mip_levels
        {
            return Err(MetalError::Invalid(
                "the view is outside the levels of the texture",
            ));
        }
        let slices = match v.kind {
            TextureKind::CubeArray => v.slice_count * 6,
            TextureKind::Cube => 6,
            _ => v.slice_count,
        };
        let first = match v.kind {
            TextureKind::CubeArray => v.first_slice * 6,
            _ => v.first_slice,
        };
        let of = match d.kind {
            TextureKind::D3 => 1,
            _ => d.slice_count(),
        };
        if first.saturating_add(slices) > of {
            return Err(MetalError::Invalid(
                "the view is outside the slices of the texture",
            ));
        }
        // SAFETY: the level and slice ranges are inside the texture (checked above).
        let raw = unsafe {
            self.inner
                .raw
                .newTextureViewWithPixelFormat_textureType_levels_slices_swizzle(
                    v.format.to_mtl(),
                    v.kind.to_mtl(),
                    NSRange::new(v.first_level as usize, v.level_count as usize),
                    NSRange::new(
                        first as usize,
                        if v.kind == TextureKind::CubeArray {
                            v.slice_count as usize * 6
                        } else {
                            slices as usize
                        },
                    ),
                    MTLTextureSwizzleChannels {
                        red: MTLTextureSwizzle(channels[0]),
                        green: MTLTextureSwizzle(channels[1]),
                        blue: MTLTextureSwizzle(channels[2]),
                        alpha: MTLTextureSwizzle(channels[3]),
                    },
                )
        }
        .ok_or_else(|| create("texture view", "newTextureView returned nil"))?;
        let [w, h, depth] = d.level_extent(v.first_level);
        let desc = TextureDesc {
            kind: v.kind,
            width: w,
            height: h,
            depth,
            mip_levels: v.level_count,
            array_layers: v.slice_count,
            allow_views: false,
            format: v.format,
            ..d
        };
        Ok(Self::wrap(
            &self.inner.ctx,
            raw,
            desc,
            false,
            Some(self.clone()),
        ))
    }

    /// Reads the pixels of level 0 into `out`, rows `bytes_per_row` apart. Only `Storage::Shared` textures can be
    /// read this way. The GPU must be done with the texture.
    pub fn read(&self, bytes_per_row: usize, out: &mut [u8]) -> Result<()> {
        let [w, h, _] = self.inner.desc.level_extent(0);
        self.read_region(
            &TextureRegion::whole(0, 0, [w, h, 1]),
            bytes_per_row,
            0,
            out,
        )
    }

    /// Reads a region into `out`. `bytes_per_image` separates the depth slices of a 3D region and is ignored
    /// when the depth is 1. Only `Storage::Shared` textures can be read this way. The GPU must be done with them.
    pub fn read_region(
        &self,
        r: &TextureRegion,
        bytes_per_row: usize,
        bytes_per_image: usize,
        out: &mut [u8],
    ) -> Result<()> {
        let d = self.inner.desc;
        if d.storage != Storage::Shared {
            return Err(MetalError::Invalid(
                "only a shared texture can be read by the CPU",
            ));
        }
        r.check(&d)?;
        let row = r.size[0] as usize * d.format.bytes_per_pixel() as usize;
        let image = bytes_per_row * r.size[1] as usize;
        let per_image = if r.size[2] > 1 {
            bytes_per_image
        } else {
            image
        };
        if bytes_per_row < row
            || per_image < image
            || out.len() < per_image * (r.size[2] as usize - 1) + image
        {
            return Err(MetalError::Invalid(
                "the output is too small for the region",
            ));
        }
        let region = MTLRegion {
            origin: MTLOrigin {
                x: r.origin[0] as usize,
                y: r.origin[1] as usize,
                z: r.origin[2] as usize,
            },
            size: MTLSize {
                width: r.size[0] as usize,
                height: r.size[1] as usize,
                depth: r.size[2] as usize,
            },
        };
        // SAFETY: `out` holds the whole region with the given strides (checked above), the region, level and
        // slice are inside the texture (`TextureRegion::check`).
        unsafe {
            self.inner
                .raw
                .getBytes_bytesPerRow_bytesPerImage_fromRegion_mipmapLevel_slice(
                    std::ptr::NonNull::from(&mut *out).cast(),
                    bytes_per_row,
                    per_image,
                    region,
                    r.level as usize,
                    r.slice as usize,
                );
        }
        Ok(())
    }
}

/// A box of pixels inside one level and slice of a texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureRegion {
    /// The mip level.
    pub level: u32,
    /// The array slice (a cube face counts as a slice). It is 0 for a 3D texture.
    pub slice: u32,
    /// The first pixel. The z value is the first depth slice of a 3D texture and 0 otherwise.
    pub origin: [u32; 3],
    /// The size in pixels. The depth is 1 unless the texture is 3D.
    pub size: [u32; 3],
}

impl TextureRegion {
    /// The whole of one level and slice.
    pub fn whole(level: u32, slice: u32, size: [u32; 3]) -> Self {
        Self {
            level,
            slice,
            origin: [0; 3],
            size,
        }
    }

    pub(crate) fn check(&self, d: &TextureDesc) -> Result<()> {
        let e = d.level_extent(self.level);
        let inside = |o: u32, s: u32, e: u32| s > 0 && o.checked_add(s).is_some_and(|end| end <= e);
        if self.level >= d.mip_levels
            || self.slice >= d.slice_count()
            || !(0..3).all(|i| inside(self.origin[i], self.size[i], e[i]))
        {
            return Err(MetalError::Invalid("the region is outside the texture"));
        }
        Ok(())
    }
}
