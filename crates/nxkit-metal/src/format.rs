//! The plain enums of the safe API. They map to Metal values in one place, so no Metal type leaves the crate.

use objc2_metal::{MTLIndexType, MTLLoadAction, MTLPixelFormat, MTLPrimitiveType, MTLStoreAction};

/// A pixel format. The list holds what the first titles need and grows with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// 8 bit unsigned integer, one channel.
    R8Uint,
    /// 16 bit unsigned integer, one channel.
    R16Uint,
    /// 32 bit unsigned integer, one channel.
    R32Uint,
    /// 32 bit unsigned integer, two channels.
    Rg32Uint,
    /// 32 bit unsigned integer, four channels.
    Rgba32Uint,
    /// 8 bit normalised, four channels.
    Rgba8Unorm,
    /// 8 bit normalised, four channels, blue first.
    Bgra8Unorm,
    /// 16 bit float, four channels.
    Rgba16Float,
    /// 32 bit float depth.
    Depth32Float,
    /// 32 bit float depth with an 8 bit stencil.
    Depth32FloatStencil8,
}

impl Format {
    pub(crate) fn to_mtl(self) -> MTLPixelFormat {
        match self {
            Self::R8Uint => MTLPixelFormat::R8Uint,
            Self::R16Uint => MTLPixelFormat::R16Uint,
            Self::R32Uint => MTLPixelFormat::R32Uint,
            Self::Rg32Uint => MTLPixelFormat::RG32Uint,
            Self::Rgba32Uint => MTLPixelFormat::RGBA32Uint,
            Self::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
            Self::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
            Self::Rgba16Float => MTLPixelFormat::RGBA16Float,
            Self::Depth32Float => MTLPixelFormat::Depth32Float,
            Self::Depth32FloatStencil8 => MTLPixelFormat::Depth32Float_Stencil8,
        }
    }

    /// The bytes of one pixel.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::R8Uint => 1,
            Self::R16Uint => 2,
            Self::R32Uint | Self::Rgba8Unorm | Self::Bgra8Unorm | Self::Depth32Float => 4,
            Self::Rg32Uint | Self::Rgba16Float | Self::Depth32FloatStencil8 => 8,
            Self::Rgba32Uint => 16,
        }
    }

    /// True for a depth format.
    pub fn is_depth(self) -> bool {
        matches!(self, Self::Depth32Float | Self::Depth32FloatStencil8)
    }

    /// True when the format has a stencil aspect.
    pub fn has_stencil(self) -> bool {
        self == Self::Depth32FloatStencil8
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
