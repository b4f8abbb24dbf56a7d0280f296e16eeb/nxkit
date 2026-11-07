//! The NVN to Metal format tables. Every format decision of the crate is in this file.
//!
//! The NVN values come from `nvn.h` of nxnative (see `THIRD_PARTY.md`). A row marked unverified has not been
//! checked against a game texture yet.

use nxkit_metal::{BcKind, Format, VertexFormat};

/// How a texture is used by the frame. It decides the Metal format and the usage flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mapped {
    /// A Metal format with the same bytes per block as NVN.
    Exact(Format),
    /// A Metal format with a different layout from NVN (depth 24 bits). The texture is a render target only. It is
    /// never deswizzled from guest memory.
    Widened(Format),
}

impl Mapped {
    pub(crate) fn format(self) -> Format {
        match self {
            Self::Exact(f) | Self::Widened(f) => f,
        }
    }
}

/// The Metal format of an NVN texture format, or `None` when Metal has no equivalent.
///
/// Three-channel formats (0x19 to 0x24) have no Metal pixel format. The UI2F formats and the packed 5-5-5-1 and 4-4-4-4
/// variants beyond the rows below are not mapped.
pub(crate) fn texture_format(nvn: i32) -> Option<Mapped> {
    use Format as F;
    let exact = |f| Some(Mapped::Exact(f));
    match nvn {
        0x01 => exact(F::R8Unorm),
        0x02 => exact(F::R8Snorm),
        0x03 => exact(F::R8Uint),
        0x04 => exact(F::R8Sint),
        0x05 => exact(F::R16Float),
        0x06 => exact(F::R16Unorm),
        0x07 => exact(F::R16Snorm),
        0x08 => exact(F::R16Uint),
        0x09 => exact(F::R16Sint),
        0x0A => exact(F::R32Float),
        0x0B => exact(F::R32Uint),
        0x0C => exact(F::R32Sint),
        0x0D => exact(F::Rg8Unorm),
        0x0E => exact(F::Rg8Snorm),
        0x0F => exact(F::Rg8Uint),
        0x10 => exact(F::Rg8Sint),
        0x11 => exact(F::Rg16Float),
        0x12 => exact(F::Rg16Unorm),
        0x13 => exact(F::Rg16Snorm),
        0x14 => exact(F::Rg16Uint),
        0x15 => exact(F::Rg16Sint),
        0x16 => exact(F::Rg32Float),
        0x17 => exact(F::Rg32Uint),
        0x18 => exact(F::Rg32Sint),
        0x25 => exact(F::Rgba8Unorm),
        0x26 => exact(F::Rgba8Snorm),
        0x27 => exact(F::Rgba8Uint),
        0x28 => exact(F::Rgba8Sint),
        0x29 => exact(F::Rgba16Float),
        0x2A => exact(F::Rgba16Unorm),
        0x2B => exact(F::Rgba16Snorm),
        0x2C => exact(F::Rgba16Uint),
        0x2D => exact(F::Rgba16Sint),
        0x2E => exact(F::Rgba32Float),
        0x2F => exact(F::Rgba32Uint),
        0x30 => exact(F::Rgba32Sint),
        0x31 => exact(F::Stencil8),
        0x32 => exact(F::Depth16Unorm),
        0x33 | 0x34 => Some(Mapped::Widened(F::Depth32Float)),
        0x35 => Some(Mapped::Widened(F::Depth32FloatStencil8)),
        0x36 => exact(F::Depth32FloatStencil8),
        0x37 | 0x38 => exact(F::Rgba8UnormSrgb),
        0x39 => exact(F::Abgr4Unorm),
        0x3D => exact(F::Rgb10A2Unorm),
        0x3E => exact(F::Rgb10A2Uint),
        0x3F => exact(F::Rg11B10Float),
        0x40 => exact(F::Rgb9E5Float),
        0x41 | 0x42 | 0x45 | 0x46 => {
            exact(F::Bc(if matches!(nvn, 0x45 | 0x46) { BcKind::Bc1Srgb } else { BcKind::Bc1 }))
        }
        0x43 => exact(F::Bc(BcKind::Bc2)),
        0x44 => exact(F::Bc(BcKind::Bc3)),
        0x47 => exact(F::Bc(BcKind::Bc2Srgb)),
        0x48 => exact(F::Bc(BcKind::Bc3Srgb)),
        0x49 => exact(F::Bc(BcKind::Bc4Unorm)),
        0x4A => exact(F::Bc(BcKind::Bc4Snorm)),
        0x4B => exact(F::Bc(BcKind::Bc5Unorm)),
        0x4C => exact(F::Bc(BcKind::Bc5Snorm)),
        0x4D => exact(F::Bc(BcKind::Bc7)),
        0x4E => exact(F::Bc(BcKind::Bc7Srgb)),
        0x4F => exact(F::Bc(BcKind::Bc6hSfloat)),
        0x50 => exact(F::Bc(BcKind::Bc6hUfloat)),
        0x79..=0x86 => exact(F::Astc {
            size: (nvn - 0x79) as u8,
            srgb: false,
        }),
        0x87..=0x94 => exact(F::Astc {
            size: (nvn - 0x87) as u8,
            srgb: true,
        }),
        0x95 => exact(F::B5G6R5Unorm),
        0x97 => exact(F::Bgr5A1Unorm),
        0x98 => exact(F::A1Bgr5Unorm),
        0x99 | 0x9A => exact(F::Bgra8Unorm),
        0x9B | 0x9C => exact(F::Bgra8UnormSrgb),
        _ => None,
    }
}

/// True for the formats that Metal cannot blend or that have integer channels. The pipeline never blends them.
pub(crate) fn is_integer(f: Format) -> bool {
    use Format as F;
    matches!(
        f,
        F::R8Uint
            | F::R8Sint
            | F::R16Uint
            | F::R16Sint
            | F::R32Uint
            | F::R32Sint
            | F::Rg8Uint
            | F::Rg8Sint
            | F::Rg16Uint
            | F::Rg16Sint
            | F::Rg32Uint
            | F::Rg32Sint
            | F::Rgba8Uint
            | F::Rgba8Sint
            | F::Rgba16Uint
            | F::Rgba16Sint
            | F::Rgba32Uint
            | F::Rgba32Sint
            | F::Rgb10A2Uint
    )
}

/// The Metal vertex format of an NVN vertex attribute format (`vfmt` of nxnative).
///
/// The one, two, three and four component groups start at 0x01, 0x0D, 0x19 and 0x25. The special formats are
/// 0x3D UInt1010102Norm, 0x69 Int1010102Norm, 0x3F FloatRG11B10 and 0x9A UChar4Norm BGRA.
pub(crate) fn vertex_format(nvn: u32) -> Option<VertexFormat> {
    use VertexFormat as V;
    match nvn {
        0x3D => return Some(V::UInt1010102Norm),
        0x69 => return Some(V::Int1010102Norm),
        0x3F => return Some(V::FloatRg11B10),
        0x9A => return Some(V::UChar4NormBgra),
        _ => {}
    }
    let (n, t) = match nvn {
        0x01..=0x30 => ((nvn - 1) / 12, (nvn - 1) % 12),
        _ => return None,
    };
    const TABLE: [[V; 4]; 12] = [
        [V::UCharNorm, V::UChar2Norm, V::UChar3Norm, V::UChar4Norm],
        [V::CharNorm, V::Char2Norm, V::Char3Norm, V::Char4Norm],
        [V::UChar, V::UChar2, V::UChar3, V::UChar4],
        [V::Char, V::Char2, V::Char3, V::Char4],
        [V::Half, V::Half2, V::Half3, V::Half4],
        [V::UShortNorm, V::UShort2Norm, V::UShort3Norm, V::UShort4Norm],
        [V::ShortNorm, V::Short2Norm, V::Short3Norm, V::Short4Norm],
        [V::UShort, V::UShort2, V::UShort3, V::UShort4],
        [V::Short, V::Short2, V::Short3, V::Short4],
        [V::Float, V::Float2, V::Float3, V::Float4],
        [V::UInt, V::UInt2, V::UInt3, V::UInt4],
        [V::Int, V::Int2, V::Int3, V::Int4],
    ];
    TABLE.get(t as usize)?.get(n as usize).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_and_depth_formats_map() {
        assert_eq!(texture_format(0x25).map(Mapped::format), Some(Format::Rgba8Unorm));
        assert_eq!(texture_format(0x35), Some(Mapped::Widened(Format::Depth32FloatStencil8)));
        assert_eq!(texture_format(0x19), None);
        assert_eq!(
            texture_format(0x80).map(Mapped::format),
            Some(Format::Astc { size: 7, srgb: false })
        );
    }

    #[test]
    fn vertex_formats_follow_the_group_layout() {
        assert_eq!(vertex_format(0x0A), Some(VertexFormat::Float));
        assert_eq!(vertex_format(0x0D + 0x09), Some(VertexFormat::Float2));
        assert_eq!(vertex_format(0x25 + 0x09), Some(VertexFormat::Float4));
        assert_eq!(vertex_format(0x25), Some(VertexFormat::UChar4Norm));
        assert_eq!(vertex_format(0x9A), Some(VertexFormat::UChar4NormBgra));
        assert_eq!(vertex_format(0x31), None);
    }
}
