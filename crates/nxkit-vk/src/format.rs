//! `VkFormat` as the backend sees it: a pixel format and a vertex format of this crate, the size of a texel or a
//! block, the aspects and whether the host must convert the texels.
//!
//! The pixel and vertex formats are named after the Metal formats that the backend will pick, but they are plain
//! enums: this crate has no Metal types. The table follows the reference layer (`hle_vk.m`, formats):
//!
//! - Metal names the parts of a packed format from the low bits, Vulkan from the high bits, so `R5G6B5_UNORM_PACK16`
//!   is [`PixelFormat::B5g6r5Unorm`].
//! - The packed formats that Metal does not have ([`Conversion`]) are stored in a wider format, and the host unpacks
//!   the texels into it. Only linear images that the CPU writes take them.
//! - `X8_D24_UNORM_PACK32` and `D24_UNORM_S8_UINT` are stored as 32-bit float depth.
//! - Three-component and scaled formats are vertex formats only.

/// A pixel format of a texture, after Metal's names.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    R8Unorm,
    R8Snorm,
    R8Uint,
    R8Sint,
    Rg8Unorm,
    Rg8Snorm,
    Rg8Uint,
    Rg8Sint,
    Rgba8Unorm,
    Rgba8Snorm,
    Rgba8Uint,
    Rgba8Sint,
    Rgba8UnormSrgb,
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgb10a2Unorm,
    Rgb10a2Uint,
    Rg11b10Float,
    Rgb9e5Float,
    B5g6r5Unorm,
    Bgr5a1Unorm,
    A1bgr5Unorm,
    Abgr4Unorm,
    R16Unorm,
    R16Snorm,
    R16Uint,
    R16Sint,
    R16Float,
    Rg16Unorm,
    Rg16Snorm,
    Rg16Uint,
    Rg16Sint,
    Rg16Float,
    Rgba16Unorm,
    Rgba16Snorm,
    Rgba16Uint,
    Rgba16Sint,
    Rgba16Float,
    R32Uint,
    R32Sint,
    R32Float,
    Rg32Uint,
    Rg32Sint,
    Rg32Float,
    Rgba32Uint,
    Rgba32Sint,
    Rgba32Float,
    Depth16Unorm,
    Depth32Float,
    Stencil8,
    Depth32FloatStencil8,
    /// The stencil of a [`PixelFormat::Depth32FloatStencil8`] texture, for a stencil-aspect view.
    X32Stencil8,
    Bc1Rgba,
    Bc1RgbaSrgb,
    Bc2Rgba,
    Bc2RgbaSrgb,
    Bc3Rgba,
    Bc3RgbaSrgb,
    Bc4RUnorm,
    Bc4RSnorm,
    Bc5RgUnorm,
    Bc5RgSnorm,
    Bc6hRgbUfloat,
    Bc6hRgbFloat,
    Bc7RgbaUnorm,
    Bc7RgbaUnormSrgb,
    Astc4x4Ldr,
    Astc4x4Srgb,
    Astc5x5Ldr,
    Astc5x5Srgb,
    Astc6x6Ldr,
    Astc6x6Srgb,
    Astc8x8Ldr,
    Astc8x8Srgb,
}

/// A vertex attribute format, after Metal's names.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VertexFormat {
    UChar,
    UChar2,
    UChar3,
    UChar4,
    Char,
    Char2,
    Char3,
    Char4,
    UCharNormalized,
    UChar2Normalized,
    UChar3Normalized,
    UChar4Normalized,
    UChar4NormalizedBgra,
    CharNormalized,
    Char2Normalized,
    Char3Normalized,
    Char4Normalized,
    UShort,
    UShort2,
    UShort3,
    UShort4,
    Short,
    Short2,
    Short3,
    Short4,
    UShortNormalized,
    UShort2Normalized,
    UShort3Normalized,
    UShort4Normalized,
    ShortNormalized,
    Short2Normalized,
    Short3Normalized,
    Short4Normalized,
    Half,
    Half2,
    Half3,
    Half4,
    UInt,
    UInt2,
    UInt3,
    UInt4,
    Int,
    Int2,
    Int3,
    Int4,
    Float,
    Float2,
    Float3,
    Float4,
    UInt1010102Normalized,
    FloatRg11b10,
    FloatRgb9e5,
}

/// What kind of data a format holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// Normalized or float colour.
    Color,
    /// Unsigned integer colour.
    UInt,
    /// Signed integer colour.
    SInt,
    /// Depth only.
    Depth,
    /// Depth and stencil.
    DepthStencil,
    /// Stencil only.
    Stencil,
    /// A compressed block format.
    Block,
}

/// A packed format that the host unpacks into the wider pixel format of the texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Conversion {
    /// The texels are stored as they are.
    None,
    /// `R4G4_UNORM_PACK8` into `Rg8Unorm`.
    R4g4,
    /// `B4G4R4A4_UNORM_PACK16` into `Rgba8Unorm`.
    B4g4r4a4,
    /// `B5G6R5_UNORM_PACK16` into `Rgba8Unorm`.
    B5g6r5,
    /// `B5G5R5A1_UNORM_PACK16` into `Rgba8Unorm`.
    B5g5r5a1,
}

/// The aspect bits of `VkImageAspectFlags`.
pub mod aspect {
    /// `VK_IMAGE_ASPECT_COLOR_BIT`.
    pub const COLOR: u32 = 1;
    /// `VK_IMAGE_ASPECT_DEPTH_BIT`.
    pub const DEPTH: u32 = 2;
    /// `VK_IMAGE_ASPECT_STENCIL_BIT`.
    pub const STENCIL: u32 = 4;
}

/// One `VkFormat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatInfo {
    /// The `VkFormat` value.
    pub vk: u32,
    /// The pixel format of a texture, None for a format that is a vertex format only.
    pub pixel: Option<PixelFormat>,
    /// The vertex format, None for a format that vertex input cannot read.
    pub vertex: Option<VertexFormat>,
    /// The bytes of one texel, or of one block for a block format.
    pub bytes: u8,
    /// The width of a block in texels (1 for a format without blocks).
    pub block_w: u8,
    /// The height of a block in texels.
    pub block_h: u8,
    /// What the format holds.
    pub class: Class,
    /// The unpacking that the host does.
    pub conversion: Conversion,
}

impl FormatInfo {
    /// Whether the format has depth.
    pub fn has_depth(&self) -> bool {
        matches!(self.class, Class::Depth | Class::DepthStencil)
    }

    /// Whether the format has stencil.
    pub fn has_stencil(&self) -> bool {
        matches!(self.class, Class::Stencil | Class::DepthStencil)
    }

    /// Whether the format is a colour format (normalized, float or integer).
    pub fn is_color(&self) -> bool {
        matches!(self.class, Class::Color | Class::UInt | Class::SInt)
    }

    /// Whether the format holds integers.
    pub fn is_int(&self) -> bool {
        matches!(self.class, Class::UInt | Class::SInt)
    }

    /// The aspects of the format ([`aspect`]).
    pub fn aspects(&self) -> u32 {
        match self.class {
            Class::Depth => aspect::DEPTH,
            Class::Stencil => aspect::STENCIL,
            Class::DepthStencil => aspect::DEPTH | aspect::STENCIL,
            _ => aspect::COLOR,
        }
    }

    /// Whether the host converts the texels.
    pub fn needs_conversion(&self) -> bool {
        self.conversion != Conversion::None
    }

    /// The bytes of one level of `w` x `h` x `d` texels, whole blocks.
    pub fn level_bytes(&self, w: u32, h: u32, d: u32) -> u64 {
        let bw = u32::from(self.block_w.max(1));
        let bh = u32::from(self.block_h.max(1));
        u64::from(w.div_ceil(bw)) * u64::from(h.div_ceil(bh)) * u64::from(d) * u64::from(self.bytes)
    }

    /// The bytes of a whole image: every level, times the layers and the samples (the reference's `img_bytes`).
    pub fn image_bytes(
        &self,
        w: u32,
        h: u32,
        d: u32,
        levels: u32,
        layers: u32,
        samples: u32,
    ) -> u64 {
        let per_layer: u64 = (0..levels.min(32))
            .map(|m| self.level_bytes((w >> m).max(1), (h >> m).max(1), (d >> m).max(1)))
            .sum();
        per_layer * u64::from(layers) * u64::from(samples)
    }

    /// The member of a `VkClearColorValue` union that this format reads: `int32` for a signed integer format, `uint32`
    /// for an unsigned one, `float32` for every other.
    pub fn clear_color(&self, raw: [u32; 4]) -> ClearColor {
        match self.class {
            Class::SInt => ClearColor::SInt(raw.map(|v| v as i32)),
            Class::UInt => ClearColor::UInt(raw),
            _ => ClearColor::Float(raw.map(f32::from_bits)),
        }
    }
}

/// A clear colour, read as the member of the union that the format names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClearColor {
    /// `float32`.
    Float([f32; 4]),
    /// `uint32`.
    UInt([u32; 4]),
    /// `int32`.
    SInt([i32; 4]),
}

use Class::{
    Block as B, Color as C, Depth as D, DepthStencil as DS, SInt as S, Stencil as ST, UInt as U,
};
use PixelFormat as P;
use VertexFormat as V;

const fn f(vk: u32, pixel: Option<P>, vertex: Option<V>, bytes: u8, class: Class) -> FormatInfo {
    FormatInfo {
        vk,
        pixel,
        vertex,
        bytes,
        block_w: 1,
        block_h: 1,
        class,
        conversion: Conversion::None,
    }
}

const fn x(vk: u32, pixel: P, bytes: u8, conversion: Conversion) -> FormatInfo {
    FormatInfo {
        vk,
        pixel: Some(pixel),
        vertex: None,
        bytes,
        block_w: 1,
        block_h: 1,
        class: C,
        conversion,
    }
}

const fn b(vk: u32, pixel: P, bytes: u8, w: u8, h: u8) -> FormatInfo {
    FormatInfo {
        vk,
        pixel: Some(pixel),
        vertex: None,
        bytes,
        block_w: w,
        block_h: h,
        class: B,
        conversion: Conversion::None,
    }
}

const fn v(vk: u32, vertex: V, bytes: u8) -> FormatInfo {
    f(vk, None, Some(vertex), bytes, C)
}

/// The formats, sorted by `VkFormat` value.
pub static FORMATS: &[FormatInfo] = &[
    x(1, P::Rg8Unorm, 1, Conversion::R4g4),
    f(2, Some(P::Abgr4Unorm), None, 2, C),
    x(3, P::Rgba8Unorm, 2, Conversion::B4g4r4a4),
    f(4, Some(P::B5g6r5Unorm), None, 2, C),
    x(5, P::Rgba8Unorm, 2, Conversion::B5g6r5),
    f(6, Some(P::A1bgr5Unorm), None, 2, C),
    x(7, P::Rgba8Unorm, 2, Conversion::B5g5r5a1),
    f(8, Some(P::Bgr5a1Unorm), None, 2, C),
    f(9, Some(P::R8Unorm), Some(V::UCharNormalized), 1, C),
    f(10, Some(P::R8Snorm), Some(V::CharNormalized), 1, C),
    v(11, V::UChar, 1),
    v(12, V::Char, 1),
    f(13, Some(P::R8Uint), Some(V::UChar), 1, U),
    f(14, Some(P::R8Sint), Some(V::Char), 1, S),
    f(16, Some(P::Rg8Unorm), Some(V::UChar2Normalized), 2, C),
    f(17, Some(P::Rg8Snorm), Some(V::Char2Normalized), 2, C),
    v(18, V::UChar2, 2),
    v(19, V::Char2, 2),
    f(20, Some(P::Rg8Uint), Some(V::UChar2), 2, U),
    f(21, Some(P::Rg8Sint), Some(V::Char2), 2, S),
    v(23, V::UChar3Normalized, 3),
    v(24, V::Char3Normalized, 3),
    v(25, V::UChar3, 3),
    v(26, V::Char3, 3),
    v(27, V::UChar3, 3),
    v(28, V::Char3, 3),
    f(37, Some(P::Rgba8Unorm), Some(V::UChar4Normalized), 4, C),
    f(38, Some(P::Rgba8Snorm), Some(V::Char4Normalized), 4, C),
    v(39, V::UChar4, 4),
    v(40, V::Char4, 4),
    f(41, Some(P::Rgba8Uint), Some(V::UChar4), 4, U),
    f(42, Some(P::Rgba8Sint), Some(V::Char4), 4, S),
    f(43, Some(P::Rgba8UnormSrgb), None, 4, C),
    f(44, Some(P::Bgra8Unorm), Some(V::UChar4NormalizedBgra), 4, C),
    f(50, Some(P::Bgra8UnormSrgb), None, 4, C),
    f(51, Some(P::Rgba8Unorm), Some(V::UChar4Normalized), 4, C),
    f(57, Some(P::Rgba8UnormSrgb), None, 4, C),
    f(
        64,
        Some(P::Rgb10a2Unorm),
        Some(V::UInt1010102Normalized),
        4,
        C,
    ),
    f(68, Some(P::Rgb10a2Uint), None, 4, U),
    f(70, Some(P::R16Unorm), Some(V::UShortNormalized), 2, C),
    f(71, Some(P::R16Snorm), Some(V::ShortNormalized), 2, C),
    v(72, V::UShort, 2),
    v(73, V::Short, 2),
    f(74, Some(P::R16Uint), Some(V::UShort), 2, U),
    f(75, Some(P::R16Sint), Some(V::Short), 2, S),
    f(76, Some(P::R16Float), Some(V::Half), 2, C),
    f(77, Some(P::Rg16Unorm), Some(V::UShort2Normalized), 4, C),
    f(78, Some(P::Rg16Snorm), Some(V::Short2Normalized), 4, C),
    v(79, V::UShort2, 4),
    v(80, V::Short2, 4),
    f(81, Some(P::Rg16Uint), Some(V::UShort2), 4, U),
    f(82, Some(P::Rg16Sint), Some(V::Short2), 4, S),
    f(83, Some(P::Rg16Float), Some(V::Half2), 4, C),
    v(84, V::UShort3Normalized, 6),
    v(85, V::Short3Normalized, 6),
    v(86, V::UShort3, 6),
    v(87, V::Short3, 6),
    v(88, V::UShort3, 6),
    v(89, V::Short3, 6),
    v(90, V::Half3, 6),
    f(91, Some(P::Rgba16Unorm), Some(V::UShort4Normalized), 8, C),
    f(92, Some(P::Rgba16Snorm), Some(V::Short4Normalized), 8, C),
    v(93, V::UShort4, 8),
    v(94, V::Short4, 8),
    f(95, Some(P::Rgba16Uint), Some(V::UShort4), 8, U),
    f(96, Some(P::Rgba16Sint), Some(V::Short4), 8, S),
    f(97, Some(P::Rgba16Float), Some(V::Half4), 8, C),
    f(98, Some(P::R32Uint), Some(V::UInt), 4, U),
    f(99, Some(P::R32Sint), Some(V::Int), 4, S),
    f(100, Some(P::R32Float), Some(V::Float), 4, C),
    f(101, Some(P::Rg32Uint), Some(V::UInt2), 8, U),
    f(102, Some(P::Rg32Sint), Some(V::Int2), 8, S),
    f(103, Some(P::Rg32Float), Some(V::Float2), 8, C),
    f(104, None, Some(V::UInt3), 12, U),
    f(105, None, Some(V::Int3), 12, S),
    f(106, None, Some(V::Float3), 12, C),
    f(107, Some(P::Rgba32Uint), Some(V::UInt4), 16, U),
    f(108, Some(P::Rgba32Sint), Some(V::Int4), 16, S),
    f(109, Some(P::Rgba32Float), Some(V::Float4), 16, C),
    f(122, Some(P::Rg11b10Float), Some(V::FloatRg11b10), 4, C),
    f(123, Some(P::Rgb9e5Float), Some(V::FloatRgb9e5), 4, C),
    f(124, Some(P::Depth16Unorm), None, 2, D),
    f(125, Some(P::Depth32Float), None, 4, D),
    f(126, Some(P::Depth32Float), None, 4, D),
    f(127, Some(P::Stencil8), None, 1, ST),
    f(129, Some(P::Depth32FloatStencil8), None, 4, DS),
    f(130, Some(P::Depth32FloatStencil8), None, 8, DS),
    b(131, P::Bc1Rgba, 8, 4, 4),
    b(132, P::Bc1RgbaSrgb, 8, 4, 4),
    b(133, P::Bc1Rgba, 8, 4, 4),
    b(134, P::Bc1RgbaSrgb, 8, 4, 4),
    b(135, P::Bc2Rgba, 16, 4, 4),
    b(136, P::Bc2RgbaSrgb, 16, 4, 4),
    b(137, P::Bc3Rgba, 16, 4, 4),
    b(138, P::Bc3RgbaSrgb, 16, 4, 4),
    b(139, P::Bc4RUnorm, 8, 4, 4),
    b(140, P::Bc4RSnorm, 8, 4, 4),
    b(141, P::Bc5RgUnorm, 16, 4, 4),
    b(142, P::Bc5RgSnorm, 16, 4, 4),
    b(143, P::Bc6hRgbUfloat, 16, 4, 4),
    b(144, P::Bc6hRgbFloat, 16, 4, 4),
    b(145, P::Bc7RgbaUnorm, 16, 4, 4),
    b(146, P::Bc7RgbaUnormSrgb, 16, 4, 4),
    b(157, P::Astc4x4Ldr, 16, 4, 4),
    b(158, P::Astc4x4Srgb, 16, 4, 4),
    b(161, P::Astc5x5Ldr, 16, 5, 5),
    b(162, P::Astc5x5Srgb, 16, 5, 5),
    b(165, P::Astc6x6Ldr, 16, 6, 6),
    b(166, P::Astc6x6Srgb, 16, 6, 6),
    b(171, P::Astc8x8Ldr, 16, 8, 8),
    b(172, P::Astc8x8Srgb, 16, 8, 8),
];

/// `VK_FORMAT_BC1_RGB_UNORM_BLOCK` and `VK_FORMAT_BC1_RGB_SRGB_BLOCK`: Metal decodes BC1 with punch-through alpha, the
/// RGB formats of Vulkan have none, so a view of them reads alpha as one.
pub fn is_bc1_rgb(vk: u32) -> bool {
    vk == 131 || vk == 132
}

/// The description of a `VkFormat`, None for a format that the front end does not know.
pub fn lookup(vk: u32) -> Option<&'static FormatInfo> {
    FORMATS
        .binary_search_by_key(&vk, |f| f.vk)
        .ok()
        .and_then(|i| FORMATS.get(i))
}

/// The description of a format that a texture can have: known, with a pixel format.
pub fn texture_format(vk: u32) -> Option<&'static FormatInfo> {
    lookup(vk).filter(|f| f.pixel.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_found() {
        assert!(FORMATS.windows(2).all(|w| w[0].vk < w[1].vk));
        let rgba = lookup(37);
        assert_eq!(rgba.and_then(|f| f.pixel), Some(P::Rgba8Unorm));
        assert_eq!(lookup(15), None);
        assert_eq!(texture_format(106), None);
        assert_eq!(lookup(106).and_then(|f| f.vertex), Some(V::Float3));
    }

    #[test]
    fn aspects_and_conversions() {
        let ds = lookup(130).map(|f| (f.aspects(), f.has_depth(), f.has_stencil()));
        assert_eq!(ds, Some((aspect::DEPTH | aspect::STENCIL, true, true)));
        assert_eq!(lookup(127).map(FormatInfo::aspects), Some(aspect::STENCIL));
        assert_eq!(lookup(5).map(FormatInfo::needs_conversion), Some(true));
        assert_eq!(lookup(4).map(FormatInfo::needs_conversion), Some(false));
        assert!(is_bc1_rgb(131) && !is_bc1_rgb(133));
    }

    #[test]
    fn sizes_count_whole_blocks_and_every_level() {
        let bc1 = lookup(133).copied();
        assert_eq!(bc1.map(|f| f.level_bytes(5, 5, 1)), Some(4 * 8));
        let rgba = lookup(37).copied();
        // 4x4 + 2x2 + 1x1 texels, 2 layers.
        assert_eq!(
            rgba.map(|f| f.image_bytes(4, 4, 1, 3, 2, 1)),
            Some((16 + 4 + 1) * 4 * 2)
        );
    }

    #[test]
    fn clear_colours_read_the_member_of_the_format() {
        let raw = [0x3F80_0000, 0, 0x4000_0000, u32::MAX];
        let float = [0x3F80_0000, 0, 0x4000_0000, 0];
        assert_eq!(
            lookup(37).map(|f| f.clear_color(float)),
            Some(ClearColor::Float([1.0, 0.0, 2.0, 0.0]))
        );
        assert_eq!(
            lookup(42).map(|f| f.clear_color(raw)),
            Some(ClearColor::SInt([0x3F80_0000, 0, 0x4000_0000, -1]))
        );
        assert_eq!(
            lookup(41).map(|f| f.clear_color(raw)),
            Some(ClearColor::UInt(raw))
        );
    }
}
