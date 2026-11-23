//! Pixel formats as plain data. `nxkit-metal` maps them to Metal, and the pipeline list names them by `Debug`.
//!
//! The order of the variants is part of the key scheme of the pipeline cache (`nxkit-gpu` `pipelines/keys.rs`): the
//! keys hash these values. Do not reorder them.

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
    ($($(#[$m:meta])* $name:ident => $metal:expr, $bytes:expr, $class:ident;)+) => {
        /// A pixel format. The list covers the formats of the NVN table ([`crate::nvn::texture_format`]).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Format {
            $($(#[$m])* $name,)+
            /// A block compressed format of the BC family.
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

            /// The value of `MTLPixelFormat` for a format that is not block compressed. `nxkit-metal` checks it
            /// against its own table.
            pub fn metal_value(self) -> Option<u32> {
                match self {
                    $(Self::$name => Some($metal),)+
                    Self::Bc(_) | Self::Astc { .. } => None,
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
    R8Uint => 13, 1, Color;
    /// 8 bit signed integer, one channel.
    R8Sint => 14, 1, Color;
    /// 8 bit normalised, one channel.
    R8Unorm => 10, 1, Color;
    /// 8 bit signed normalised, one channel.
    R8Snorm => 12, 1, Color;
    /// 16 bit unsigned integer, one channel.
    R16Uint => 23, 2, Color;
    /// 16 bit signed integer, one channel.
    R16Sint => 24, 2, Color;
    /// 16 bit normalised, one channel.
    R16Unorm => 20, 2, Color;
    /// 16 bit signed normalised, one channel.
    R16Snorm => 22, 2, Color;
    /// 16 bit float, one channel.
    R16Float => 25, 2, Color;
    /// 32 bit unsigned integer, one channel.
    R32Uint => 53, 4, Color;
    /// 32 bit signed integer, one channel.
    R32Sint => 54, 4, Color;
    /// 32 bit float, one channel.
    R32Float => 55, 4, Color;
    /// 8 bit unsigned integer, two channels.
    Rg8Uint => 33, 2, Color;
    /// 8 bit signed integer, two channels.
    Rg8Sint => 34, 2, Color;
    /// 8 bit normalised, two channels.
    Rg8Unorm => 30, 2, Color;
    /// 8 bit signed normalised, two channels.
    Rg8Snorm => 32, 2, Color;
    /// 16 bit unsigned integer, two channels.
    Rg16Uint => 63, 4, Color;
    /// 16 bit signed integer, two channels.
    Rg16Sint => 64, 4, Color;
    /// 16 bit normalised, two channels.
    Rg16Unorm => 60, 4, Color;
    /// 16 bit signed normalised, two channels.
    Rg16Snorm => 62, 4, Color;
    /// 16 bit float, two channels.
    Rg16Float => 65, 4, Color;
    /// 32 bit unsigned integer, two channels.
    Rg32Uint => 103, 8, Color;
    /// 32 bit signed integer, two channels.
    Rg32Sint => 104, 8, Color;
    /// 32 bit float, two channels.
    Rg32Float => 105, 8, Color;
    /// 8 bit unsigned integer, four channels.
    Rgba8Uint => 73, 4, Color;
    /// 8 bit signed integer, four channels.
    Rgba8Sint => 74, 4, Color;
    /// 8 bit normalised, four channels.
    Rgba8Unorm => 70, 4, Color;
    /// 8 bit normalised sRGB, four channels.
    Rgba8UnormSrgb => 71, 4, Color;
    /// 8 bit signed normalised, four channels.
    Rgba8Snorm => 72, 4, Color;
    /// 8 bit normalised, four channels, blue first.
    Bgra8Unorm => 80, 4, Color;
    /// 8 bit normalised sRGB, four channels, blue first.
    Bgra8UnormSrgb => 81, 4, Color;
    /// 16 bit unsigned integer, four channels.
    Rgba16Uint => 113, 8, Color;
    /// 16 bit signed integer, four channels.
    Rgba16Sint => 114, 8, Color;
    /// 16 bit normalised, four channels.
    Rgba16Unorm => 110, 8, Color;
    /// 16 bit signed normalised, four channels.
    Rgba16Snorm => 112, 8, Color;
    /// 16 bit float, four channels.
    Rgba16Float => 115, 8, Color;
    /// 32 bit unsigned integer, four channels.
    Rgba32Uint => 123, 16, Color;
    /// 32 bit signed integer, four channels.
    Rgba32Sint => 124, 16, Color;
    /// 32 bit float, four channels.
    Rgba32Float => 125, 16, Color;
    /// 10-10-10-2 normalised.
    Rgb10A2Unorm => 90, 4, Color;
    /// 10-10-10-2 unsigned integer.
    Rgb10A2Uint => 91, 4, Color;
    /// 11-11-10 float.
    Rg11B10Float => 92, 4, Color;
    /// 9-9-9 float with a shared 5 bit exponent.
    Rgb9E5Float => 93, 4, Color;
    /// 5-6-5 normalised, blue in the low bits.
    B5G6R5Unorm => 40, 2, Color;
    /// 5-5-5-1 normalised, blue in the low bits.
    Bgr5A1Unorm => 43, 2, Color;
    /// 1-5-5-5 normalised, blue in the low bits.
    A1Bgr5Unorm => 41, 2, Color;
    /// 4-4-4-4 normalised.
    Abgr4Unorm => 42, 2, Color;
    /// 16 bit normalised depth.
    Depth16Unorm => 250, 2, Depth;
    /// 32 bit float depth.
    Depth32Float => 252, 4, Depth;
    /// 8 bit stencil.
    Stencil8 => 253, 1, Stencil;
    /// 32 bit float depth with an 8 bit stencil.
    Depth32FloatStencil8 => 260, 8, DepthStencil;
    /// The stencil of a [`Format::Depth32FloatStencil8`] texture, for a view that reads the stencil
    /// (`NVN_TEXTURE_DEPTH_STENCIL_MODE_STENCIL`). Only a view has it, never a texture.
    X32Stencil8 => 261, 8, Stencil;
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
            | Self::Stencil8
            | Self::X32Stencil8 => SampleType::Uint,
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

impl Format {
    /// The format whose `MTLPixelFormat` value is `v`, among the formats that are not block compressed.
    pub fn from_metal_value(v: u32) -> Option<Self> {
        Self::SIMPLE
            .iter()
            .copied()
            .find(|f| f.metal_value() == Some(v))
    }
}
