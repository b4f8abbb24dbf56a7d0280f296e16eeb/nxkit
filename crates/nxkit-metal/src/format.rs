//! The plain enums of the safe API. They map to Metal values in one place, so no Metal type leaves the crate.
//!
//! The pixel formats are `nxkit-pipestate`'s ([`Format`]), shared with the build. This module maps them to Metal.

use objc2_metal::{MTLIndexType, MTLLoadAction, MTLPixelFormat, MTLPrimitiveType, MTLStoreAction};

pub use nxkit_pipestate::{BcKind, Format, FormatClass, SampleType};

use crate::convert::Mtl;

impl Mtl for Format {
    type Out = MTLPixelFormat;

    fn to_mtl(self) -> MTLPixelFormat {
        match self {
            Format::R8Uint => MTLPixelFormat::R8Uint,
            Format::R8Sint => MTLPixelFormat::R8Sint,
            Format::R8Unorm => MTLPixelFormat::R8Unorm,
            Format::R8Snorm => MTLPixelFormat::R8Snorm,
            Format::R16Uint => MTLPixelFormat::R16Uint,
            Format::R16Sint => MTLPixelFormat::R16Sint,
            Format::R16Unorm => MTLPixelFormat::R16Unorm,
            Format::R16Snorm => MTLPixelFormat::R16Snorm,
            Format::R16Float => MTLPixelFormat::R16Float,
            Format::R32Uint => MTLPixelFormat::R32Uint,
            Format::R32Sint => MTLPixelFormat::R32Sint,
            Format::R32Float => MTLPixelFormat::R32Float,
            Format::Rg8Uint => MTLPixelFormat::RG8Uint,
            Format::Rg8Sint => MTLPixelFormat::RG8Sint,
            Format::Rg8Unorm => MTLPixelFormat::RG8Unorm,
            Format::Rg8Snorm => MTLPixelFormat::RG8Snorm,
            Format::Rg16Uint => MTLPixelFormat::RG16Uint,
            Format::Rg16Sint => MTLPixelFormat::RG16Sint,
            Format::Rg16Unorm => MTLPixelFormat::RG16Unorm,
            Format::Rg16Snorm => MTLPixelFormat::RG16Snorm,
            Format::Rg16Float => MTLPixelFormat::RG16Float,
            Format::Rg32Uint => MTLPixelFormat::RG32Uint,
            Format::Rg32Sint => MTLPixelFormat::RG32Sint,
            Format::Rg32Float => MTLPixelFormat::RG32Float,
            Format::Rgba8Uint => MTLPixelFormat::RGBA8Uint,
            Format::Rgba8Sint => MTLPixelFormat::RGBA8Sint,
            Format::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
            Format::Rgba8UnormSrgb => MTLPixelFormat::RGBA8Unorm_sRGB,
            Format::Rgba8Snorm => MTLPixelFormat::RGBA8Snorm,
            Format::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
            Format::Bgra8UnormSrgb => MTLPixelFormat::BGRA8Unorm_sRGB,
            Format::Rgba16Uint => MTLPixelFormat::RGBA16Uint,
            Format::Rgba16Sint => MTLPixelFormat::RGBA16Sint,
            Format::Rgba16Unorm => MTLPixelFormat::RGBA16Unorm,
            Format::Rgba16Snorm => MTLPixelFormat::RGBA16Snorm,
            Format::Rgba16Float => MTLPixelFormat::RGBA16Float,
            Format::Rgba32Uint => MTLPixelFormat::RGBA32Uint,
            Format::Rgba32Sint => MTLPixelFormat::RGBA32Sint,
            Format::Rgba32Float => MTLPixelFormat::RGBA32Float,
            Format::Rgb10A2Unorm => MTLPixelFormat::RGB10A2Unorm,
            Format::Rgb10A2Uint => MTLPixelFormat::RGB10A2Uint,
            Format::Rg11B10Float => MTLPixelFormat::RG11B10Float,
            Format::Rgb9E5Float => MTLPixelFormat::RGB9E5Float,
            Format::B5G6R5Unorm => MTLPixelFormat::B5G6R5Unorm,
            Format::Bgr5A1Unorm => MTLPixelFormat::BGR5A1Unorm,
            Format::A1Bgr5Unorm => MTLPixelFormat::A1BGR5Unorm,
            Format::Abgr4Unorm => MTLPixelFormat::ABGR4Unorm,
            Format::Depth16Unorm => MTLPixelFormat::Depth16Unorm,
            Format::Depth32Float => MTLPixelFormat::Depth32Float,
            Format::Stencil8 => MTLPixelFormat::Stencil8,
            Format::Depth32FloatStencil8 => MTLPixelFormat::Depth32Float_Stencil8,
            Format::X32Stencil8 => MTLPixelFormat::X32_Stencil8,
            Format::Bc(k) => bc_mtl(k),
            Format::Astc { size, srgb } => astc_mtl(size, srgb),
        }
    }
}

fn bc_mtl(k: BcKind) -> MTLPixelFormat {
    match k {
        BcKind::Bc1 => MTLPixelFormat::BC1_RGBA,
        BcKind::Bc1Srgb => MTLPixelFormat::BC1_RGBA_sRGB,
        BcKind::Bc2 => MTLPixelFormat::BC2_RGBA,
        BcKind::Bc2Srgb => MTLPixelFormat::BC2_RGBA_sRGB,
        BcKind::Bc3 => MTLPixelFormat::BC3_RGBA,
        BcKind::Bc3Srgb => MTLPixelFormat::BC3_RGBA_sRGB,
        BcKind::Bc4Unorm => MTLPixelFormat::BC4_RUnorm,
        BcKind::Bc4Snorm => MTLPixelFormat::BC4_RSnorm,
        BcKind::Bc5Unorm => MTLPixelFormat::BC5_RGUnorm,
        BcKind::Bc5Snorm => MTLPixelFormat::BC5_RGSnorm,
        BcKind::Bc6hSfloat => MTLPixelFormat::BC6H_RGBFloat,
        BcKind::Bc6hUfloat => MTLPixelFormat::BC6H_RGBUfloat,
        BcKind::Bc7 => MTLPixelFormat::BC7_RGBAUnorm,
        BcKind::Bc7Srgb => MTLPixelFormat::BC7_RGBAUnorm_sRGB,
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_metal_values_of_the_shared_table_are_metals() {
        for f in Format::SIMPLE {
            assert_eq!(f.metal_value(), Some(f.to_mtl().0 as u32), "{f:?}");
            assert_eq!(Format::from_metal_value(f.to_mtl().0 as u32), Some(*f));
        }
    }
}
