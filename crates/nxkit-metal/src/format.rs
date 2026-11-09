//! The plain enums of the safe API. They map to Metal values in one place, so no Metal type leaves the crate.

use objc2_metal::{MTLIndexType, MTLLoadAction, MTLPixelFormat, MTLPrimitiveType, MTLStoreAction};

/// The type a shader sees when it reads a colour format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleType {
    /// Float, normalised or sRGB values.
    Float,
    /// Unsigned integers.
    Uint,
    /// Signed integers.
    Sint,
}

/// What a pixel format stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormatClass {
    /// Colour.
    Color,
    /// Depth only.
    Depth,
    /// Stencil only.
    Stencil,
    /// Depth and stencil.
    DepthStencil,
}

macro_rules! formats {
    ($($(#[$m:meta])* $name:ident => $mtl:ident, $bytes:expr, $class:ident;)+) => {
        /// A pixel format. The list covers the formats of the NVN table in `nxkit-gpu`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Format {
            $($(#[$m])* $name,)+
            /// A block compressed format of the BC family, named by its Metal format.
            Bc(BcKind),
            /// An ASTC format. `size` indexes the block sizes 4x4, 5x4, 5x5, 6x5, 6x6, 8x5, 8x6, 8x8, 10x5, 10x6,
            /// 10x8, 10x10, 12x10, 12x12.
            Astc {
                /// The index of the block size, 0 to 13.
                size: u8,
                /// True for the sRGB variant, false for LDR.
                srgb: bool,
            },
        }

        impl Format {
            /// Every format that is not block compressed, in declaration order (for a text form that is read back).
            pub const SIMPLE: &'static [Self] = &[$(Self::$name),+];

            pub(crate) fn to_mtl(self) -> MTLPixelFormat {
                match self {
                    $(Self::$name => MTLPixelFormat::$mtl,)+
                    Self::Bc(k) => k.to_mtl(),
                    Self::Astc { size, srgb } => astc_mtl(size, srgb),
                }
            }

            /// The bytes of one block. A block is one pixel for formats that are not compressed.
            pub fn block_bytes(self) -> u32 {
                match self {
                    $(Self::$name => $bytes,)+
                    Self::Bc(k) => k.block_bytes(),
                    Self::Astc { .. } => 16,
                }
            }

            /// What the format stores.
            pub fn class(self) -> FormatClass {
                match self {
                    $(Self::$name => FormatClass::$class,)+
                    Self::Bc(_) | Self::Astc { .. } => FormatClass::Color,
                }
            }
        }
    };
}

formats! {
    /// 8 bit unsigned integer, one channel.
    R8Uint => R8Uint, 1, Color;
    /// 8 bit signed integer, one channel.
    R8Sint => R8Sint, 1, Color;
    /// 8 bit normalised, one channel.
    R8Unorm => R8Unorm, 1, Color;
    /// 8 bit signed normalised, one channel.
    R8Snorm => R8Snorm, 1, Color;
    /// 16 bit unsigned integer, one channel.
    R16Uint => R16Uint, 2, Color;
    /// 16 bit signed integer, one channel.
    R16Sint => R16Sint, 2, Color;
    /// 16 bit normalised, one channel.
    R16Unorm => R16Unorm, 2, Color;
    /// 16 bit signed normalised, one channel.
    R16Snorm => R16Snorm, 2, Color;
    /// 16 bit float, one channel.
    R16Float => R16Float, 2, Color;
    /// 32 bit unsigned integer, one channel.
    R32Uint => R32Uint, 4, Color;
    /// 32 bit signed integer, one channel.
    R32Sint => R32Sint, 4, Color;
    /// 32 bit float, one channel.
    R32Float => R32Float, 4, Color;
    /// 8 bit unsigned integer, two channels.
    Rg8Uint => RG8Uint, 2, Color;
    /// 8 bit signed integer, two channels.
    Rg8Sint => RG8Sint, 2, Color;
    /// 8 bit normalised, two channels.
    Rg8Unorm => RG8Unorm, 2, Color;
    /// 8 bit signed normalised, two channels.
    Rg8Snorm => RG8Snorm, 2, Color;
    /// 16 bit unsigned integer, two channels.
    Rg16Uint => RG16Uint, 4, Color;
    /// 16 bit signed integer, two channels.
    Rg16Sint => RG16Sint, 4, Color;
    /// 16 bit normalised, two channels.
    Rg16Unorm => RG16Unorm, 4, Color;
    /// 16 bit signed normalised, two channels.
    Rg16Snorm => RG16Snorm, 4, Color;
    /// 16 bit float, two channels.
    Rg16Float => RG16Float, 4, Color;
    /// 32 bit unsigned integer, two channels.
    Rg32Uint => RG32Uint, 8, Color;
    /// 32 bit signed integer, two channels.
    Rg32Sint => RG32Sint, 8, Color;
    /// 32 bit float, two channels.
    Rg32Float => RG32Float, 8, Color;
    /// 8 bit unsigned integer, four channels.
    Rgba8Uint => RGBA8Uint, 4, Color;
    /// 8 bit signed integer, four channels.
    Rgba8Sint => RGBA8Sint, 4, Color;
    /// 8 bit normalised, four channels.
    Rgba8Unorm => RGBA8Unorm, 4, Color;
    /// 8 bit normalised sRGB, four channels.
    Rgba8UnormSrgb => RGBA8Unorm_sRGB, 4, Color;
    /// 8 bit signed normalised, four channels.
    Rgba8Snorm => RGBA8Snorm, 4, Color;
    /// 8 bit normalised, four channels, blue first.
    Bgra8Unorm => BGRA8Unorm, 4, Color;
    /// 8 bit normalised sRGB, four channels, blue first.
    Bgra8UnormSrgb => BGRA8Unorm_sRGB, 4, Color;
    /// 16 bit unsigned integer, four channels.
    Rgba16Uint => RGBA16Uint, 8, Color;
    /// 16 bit signed integer, four channels.
    Rgba16Sint => RGBA16Sint, 8, Color;
    /// 16 bit normalised, four channels.
    Rgba16Unorm => RGBA16Unorm, 8, Color;
    /// 16 bit signed normalised, four channels.
    Rgba16Snorm => RGBA16Snorm, 8, Color;
    /// 16 bit float, four channels.
    Rgba16Float => RGBA16Float, 8, Color;
    /// 32 bit unsigned integer, four channels.
    Rgba32Uint => RGBA32Uint, 16, Color;
    /// 32 bit signed integer, four channels.
    Rgba32Sint => RGBA32Sint, 16, Color;
    /// 32 bit float, four channels.
    Rgba32Float => RGBA32Float, 16, Color;
    /// 10-10-10-2 normalised.
    Rgb10A2Unorm => RGB10A2Unorm, 4, Color;
    /// 10-10-10-2 unsigned integer.
    Rgb10A2Uint => RGB10A2Uint, 4, Color;
    /// 11-11-10 float.
    Rg11B10Float => RG11B10Float, 4, Color;
    /// 9-9-9 float with a shared 5 bit exponent.
    Rgb9E5Float => RGB9E5Float, 4, Color;
    /// 5-6-5 normalised, blue in the low bits.
    B5G6R5Unorm => B5G6R5Unorm, 2, Color;
    /// 5-5-5-1 normalised, blue in the low bits.
    Bgr5A1Unorm => BGR5A1Unorm, 2, Color;
    /// 1-5-5-5 normalised, blue in the low bits.
    A1Bgr5Unorm => A1BGR5Unorm, 2, Color;
    /// 4-4-4-4 normalised.
    Abgr4Unorm => ABGR4Unorm, 2, Color;
    /// 16 bit normalised depth.
    Depth16Unorm => Depth16Unorm, 2, Depth;
    /// 32 bit float depth.
    Depth32Float => Depth32Float, 4, Depth;
    /// 8 bit stencil.
    Stencil8 => Stencil8, 1, Stencil;
    /// 32 bit float depth with an 8 bit stencil.
    Depth32FloatStencil8 => Depth32Float_Stencil8, 8, DepthStencil;
}

/// The block compressed formats of the BC family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BcKind {
    /// BC1.
    Bc1,
    /// BC1, sRGB.
    Bc1Srgb,
    /// BC2.
    Bc2,
    /// BC2, sRGB.
    Bc2Srgb,
    /// BC3.
    Bc3,
    /// BC3, sRGB.
    Bc3Srgb,
    /// BC4, unsigned.
    Bc4Unorm,
    /// BC4, signed.
    Bc4Snorm,
    /// BC5, unsigned.
    Bc5Unorm,
    /// BC5, signed.
    Bc5Snorm,
    /// BC6H, signed float.
    Bc6hSfloat,
    /// BC6H, unsigned float.
    Bc6hUfloat,
    /// BC7.
    Bc7,
    /// BC7, sRGB.
    Bc7Srgb,
}

impl BcKind {
    fn to_mtl(self) -> MTLPixelFormat {
        match self {
            Self::Bc1 => MTLPixelFormat::BC1_RGBA,
            Self::Bc1Srgb => MTLPixelFormat::BC1_RGBA_sRGB,
            Self::Bc2 => MTLPixelFormat::BC2_RGBA,
            Self::Bc2Srgb => MTLPixelFormat::BC2_RGBA_sRGB,
            Self::Bc3 => MTLPixelFormat::BC3_RGBA,
            Self::Bc3Srgb => MTLPixelFormat::BC3_RGBA_sRGB,
            Self::Bc4Unorm => MTLPixelFormat::BC4_RUnorm,
            Self::Bc4Snorm => MTLPixelFormat::BC4_RSnorm,
            Self::Bc5Unorm => MTLPixelFormat::BC5_RGUnorm,
            Self::Bc5Snorm => MTLPixelFormat::BC5_RGSnorm,
            Self::Bc6hSfloat => MTLPixelFormat::BC6H_RGBFloat,
            Self::Bc6hUfloat => MTLPixelFormat::BC6H_RGBUfloat,
            Self::Bc7 => MTLPixelFormat::BC7_RGBAUnorm,
            Self::Bc7Srgb => MTLPixelFormat::BC7_RGBAUnorm_sRGB,
        }
    }

    fn block_bytes(self) -> u32 {
        match self {
            Self::Bc1 | Self::Bc1Srgb | Self::Bc4Unorm | Self::Bc4Snorm => 8,
            _ => 16,
        }
    }
}

/// The ASTC block sizes, by the `size` index of `Format::Astc`.
const ASTC_BLOCKS: [(u32, u32); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

const ASTC_LDR: [MTLPixelFormat; 14] = [
    MTLPixelFormat::ASTC_4x4_LDR,
    MTLPixelFormat::ASTC_5x4_LDR,
    MTLPixelFormat::ASTC_5x5_LDR,
    MTLPixelFormat::ASTC_6x5_LDR,
    MTLPixelFormat::ASTC_6x6_LDR,
    MTLPixelFormat::ASTC_8x5_LDR,
    MTLPixelFormat::ASTC_8x6_LDR,
    MTLPixelFormat::ASTC_8x8_LDR,
    MTLPixelFormat::ASTC_10x5_LDR,
    MTLPixelFormat::ASTC_10x6_LDR,
    MTLPixelFormat::ASTC_10x8_LDR,
    MTLPixelFormat::ASTC_10x10_LDR,
    MTLPixelFormat::ASTC_12x10_LDR,
    MTLPixelFormat::ASTC_12x12_LDR,
];

const ASTC_SRGB: [MTLPixelFormat; 14] = [
    MTLPixelFormat::ASTC_4x4_sRGB,
    MTLPixelFormat::ASTC_5x4_sRGB,
    MTLPixelFormat::ASTC_5x5_sRGB,
    MTLPixelFormat::ASTC_6x5_sRGB,
    MTLPixelFormat::ASTC_6x6_sRGB,
    MTLPixelFormat::ASTC_8x5_sRGB,
    MTLPixelFormat::ASTC_8x6_sRGB,
    MTLPixelFormat::ASTC_8x8_sRGB,
    MTLPixelFormat::ASTC_10x5_sRGB,
    MTLPixelFormat::ASTC_10x6_sRGB,
    MTLPixelFormat::ASTC_10x8_sRGB,
    MTLPixelFormat::ASTC_10x10_sRGB,
    MTLPixelFormat::ASTC_12x10_sRGB,
    MTLPixelFormat::ASTC_12x12_sRGB,
];

fn astc_mtl(size: u8, srgb: bool) -> MTLPixelFormat {
    let i = usize::from(size).min(13);
    if srgb { ASTC_SRGB[i] } else { ASTC_LDR[i] }
}

impl Format {
    /// The bytes of one block. This is the pixel size for formats that are not compressed. The name is kept for them.
    pub fn bytes_per_pixel(self) -> u32 {
        self.block_bytes()
    }

    /// The width and height of one block in pixels. It is 1 by 1 for formats that are not compressed.
    pub fn block_dim(self) -> (u32, u32) {
        match self {
            Self::Bc(_) => (4, 4),
            Self::Astc { size, .. } => ASTC_BLOCKS[usize::from(size).min(13)],
            _ => (1, 1),
        }
    }

    /// The type a shader sees when it reads the format. Stencil counts as unsigned.
    pub fn sample_type(self) -> SampleType {
        match self {
            Self::R8Uint
            | Self::R16Uint
            | Self::R32Uint
            | Self::Rg8Uint
            | Self::Rg16Uint
            | Self::Rg32Uint
            | Self::Rgba8Uint
            | Self::Rgba16Uint
            | Self::Rgba32Uint
            | Self::Rgb10A2Uint
            | Self::Stencil8 => SampleType::Uint,
            Self::R8Sint
            | Self::R16Sint
            | Self::R32Sint
            | Self::Rg8Sint
            | Self::Rg16Sint
            | Self::Rg32Sint
            | Self::Rgba8Sint
            | Self::Rgba16Sint
            | Self::Rgba32Sint => SampleType::Sint,
            _ => SampleType::Float,
        }
    }

    /// True for a depth format, with or without stencil.
    pub fn is_depth(self) -> bool {
        matches!(self.class(), FormatClass::Depth | FormatClass::DepthStencil)
    }

    /// True when the format has a stencil aspect.
    pub fn has_stencil(self) -> bool {
        matches!(
            self.class(),
            FormatClass::Stencil | FormatClass::DepthStencil
        )
    }

    /// True when the format is block compressed.
    pub fn is_compressed(self) -> bool {
        matches!(self, Self::Bc(_) | Self::Astc { .. })
    }

    /// The unsigned integer format with the same pixel size, which the deswizzle kernel writes.
    pub fn uint_for_block_bytes(bytes: u32) -> Option<Self> {
        match bytes {
            1 => Some(Self::R8Uint),
            2 => Some(Self::R16Uint),
            4 => Some(Self::R32Uint),
            8 => Some(Self::Rg32Uint),
            16 => Some(Self::Rgba32Uint),
            _ => None,
        }
    }
}

/// What a render pass does with an attachment when it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadAction {
    /// The old content is not needed.
    DontCare,
    /// The old content is kept.
    Load,
    /// The attachment is cleared to the clear value.
    Clear,
}

impl LoadAction {
    pub(crate) fn to_mtl(self) -> MTLLoadAction {
        match self {
            Self::DontCare => MTLLoadAction::DontCare,
            Self::Load => MTLLoadAction::Load,
            Self::Clear => MTLLoadAction::Clear,
        }
    }
}

/// What a render pass does with an attachment when it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreAction {
    /// The content is dropped. It stays in tile memory only.
    DontCare,
    /// The content is written to the texture.
    Store,
}

impl StoreAction {
    pub(crate) fn to_mtl(self) -> MTLStoreAction {
        match self {
            Self::DontCare => MTLStoreAction::DontCare,
            Self::Store => MTLStoreAction::Store,
        }
    }
}

/// The primitive topology of a draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primitive {
    /// Points.
    Point,
    /// Lines.
    Line,
    /// A line strip.
    LineStrip,
    /// Triangles.
    Triangle,
    /// A triangle strip.
    TriangleStrip,
}

impl Primitive {
    pub(crate) fn to_mtl(self) -> MTLPrimitiveType {
        match self {
            Self::Point => MTLPrimitiveType::Point,
            Self::Line => MTLPrimitiveType::Line,
            Self::LineStrip => MTLPrimitiveType::LineStrip,
            Self::Triangle => MTLPrimitiveType::Triangle,
            Self::TriangleStrip => MTLPrimitiveType::TriangleStrip,
        }
    }
}

/// The width of an index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexType {
    /// 16 bit indices.
    U16,
    /// 32 bit indices.
    U32,
}

impl IndexType {
    pub(crate) fn to_mtl(self) -> MTLIndexType {
        match self {
            Self::U16 => MTLIndexType::UInt16,
            Self::U32 => MTLIndexType::UInt32,
        }
    }

    /// The bytes of one index.
    pub fn bytes(self) -> u64 {
        match self {
            Self::U16 => 2,
            Self::U32 => 4,
        }
    }
}
