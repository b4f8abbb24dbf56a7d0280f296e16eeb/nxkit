//! The descriptions of a texture: where it lives, its kind, its size and the views and regions of it.

use objc2_metal::MTLTextureType;

use crate::error::{MetalError, Result};
use crate::format::{Format, FormatClass};

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
    pub(super) fn to_mtl(self) -> MTLTextureType {
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
    /// Shaders run image atomics on the texture (`Texture::atomic_view`). Metal runs texture atomics only on
    /// `R32Uint` and `R32Sint` textures made with the atomic usage. A texture of one of those formats gets the usage
    /// whenever `shader_write` is set. A texture of another 4 byte colour format is stored as `R32Uint` with `format`
    /// as a view over it, so the same bytes serve both. Needs `shader_write`, `allow_views` and `Storage::Private`.
    pub shader_atomic: bool,
}

impl TextureDesc {
    /// True when `shader_atomic` stores this texture as `R32Uint` under a view in its own format.
    pub(super) fn atomic_alias(&self) -> bool {
        self.shader_atomic
            && self.shader_write
            && self.allow_views
            && self.storage == Storage::Private
            && matches!(self.kind, TextureKind::D2 | TextureKind::D2Array)
            && self.format.class() == FormatClass::Color
            && !self.format.is_compressed()
            && self.format.bytes_per_pixel() == 4
            && !matches!(self.format, Format::R32Uint | Format::R32Sint)
    }
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
            shader_atomic: false,
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
            shader_atomic: false,
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

    pub(super) fn validate(&self) -> Result<()> {
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
