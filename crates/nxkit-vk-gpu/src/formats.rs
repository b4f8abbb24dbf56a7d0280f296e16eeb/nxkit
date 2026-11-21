//! The formats of `nxkit-vk` as the formats of `nxkit-metal`: pixel formats, vertex formats, texture kinds, swizzles
//! and samplers (the reference layer's format table and `addr_mode`).

use nxkit_metal::{BcKind, Border, Filter, Format, MipFilter, VertexFormat as MV, Wrap};
use nxkit_vk::desc::{SamplerDesc, Swizzle, TextureKind};
use nxkit_vk::format::{PixelFormat as P, VertexFormat as V};

use crate::fixed::compare;

/// The Metal format of a pixel format. None for the stencil view of a depth-stencil texture (`X32Stencil8`), which
/// `nxkit-metal` has no format for.
pub(crate) fn pixel(p: P) -> Option<Format> {
    let astc = |size: u8, srgb: bool| Format::Astc { size, srgb };
    Some(match p {
        P::R8Unorm => Format::R8Unorm,
        P::R8Snorm => Format::R8Snorm,
        P::R8Uint => Format::R8Uint,
        P::R8Sint => Format::R8Sint,
        P::Rg8Unorm => Format::Rg8Unorm,
        P::Rg8Snorm => Format::Rg8Snorm,
        P::Rg8Uint => Format::Rg8Uint,
        P::Rg8Sint => Format::Rg8Sint,
        P::Rgba8Unorm => Format::Rgba8Unorm,
        P::Rgba8Snorm => Format::Rgba8Snorm,
        P::Rgba8Uint => Format::Rgba8Uint,
        P::Rgba8Sint => Format::Rgba8Sint,
        P::Rgba8UnormSrgb => Format::Rgba8UnormSrgb,
        P::Bgra8Unorm => Format::Bgra8Unorm,
        P::Bgra8UnormSrgb => Format::Bgra8UnormSrgb,
        P::Rgb10a2Unorm => Format::Rgb10A2Unorm,
        P::Rgb10a2Uint => Format::Rgb10A2Uint,
        P::Rg11b10Float => Format::Rg11B10Float,
        P::Rgb9e5Float => Format::Rgb9E5Float,
        P::B5g6r5Unorm => Format::B5G6R5Unorm,
        P::Bgr5a1Unorm => Format::Bgr5A1Unorm,
        P::A1bgr5Unorm => Format::A1Bgr5Unorm,
        P::Abgr4Unorm => Format::Abgr4Unorm,
        P::R16Unorm => Format::R16Unorm,
        P::R16Snorm => Format::R16Snorm,
        P::R16Uint => Format::R16Uint,
        P::R16Sint => Format::R16Sint,
        P::R16Float => Format::R16Float,
        P::Rg16Unorm => Format::Rg16Unorm,
        P::Rg16Snorm => Format::Rg16Snorm,
        P::Rg16Uint => Format::Rg16Uint,
        P::Rg16Sint => Format::Rg16Sint,
        P::Rg16Float => Format::Rg16Float,
        P::Rgba16Unorm => Format::Rgba16Unorm,
        P::Rgba16Snorm => Format::Rgba16Snorm,
        P::Rgba16Uint => Format::Rgba16Uint,
        P::Rgba16Sint => Format::Rgba16Sint,
        P::Rgba16Float => Format::Rgba16Float,
        P::R32Uint => Format::R32Uint,
        P::R32Sint => Format::R32Sint,
        P::R32Float => Format::R32Float,
        P::Rg32Uint => Format::Rg32Uint,
        P::Rg32Sint => Format::Rg32Sint,
        P::Rg32Float => Format::Rg32Float,
        P::Rgba32Uint => Format::Rgba32Uint,
        P::Rgba32Sint => Format::Rgba32Sint,
        P::Rgba32Float => Format::Rgba32Float,
        P::Depth16Unorm => Format::Depth16Unorm,
        P::Depth32Float => Format::Depth32Float,
        P::Stencil8 => Format::Stencil8,
        P::Depth32FloatStencil8 => Format::Depth32FloatStencil8,
        P::X32Stencil8 => return None,
        P::Bc1Rgba => Format::Bc(BcKind::Bc1),
        P::Bc1RgbaSrgb => Format::Bc(BcKind::Bc1Srgb),
        P::Bc2Rgba => Format::Bc(BcKind::Bc2),
        P::Bc2RgbaSrgb => Format::Bc(BcKind::Bc2Srgb),
        P::Bc3Rgba => Format::Bc(BcKind::Bc3),
        P::Bc3RgbaSrgb => Format::Bc(BcKind::Bc3Srgb),
        P::Bc4RUnorm => Format::Bc(BcKind::Bc4Unorm),
        P::Bc4RSnorm => Format::Bc(BcKind::Bc4Snorm),
        P::Bc5RgUnorm => Format::Bc(BcKind::Bc5Unorm),
        P::Bc5RgSnorm => Format::Bc(BcKind::Bc5Snorm),
        P::Bc6hRgbUfloat => Format::Bc(BcKind::Bc6hUfloat),
        P::Bc6hRgbFloat => Format::Bc(BcKind::Bc6hSfloat),
        P::Bc7RgbaUnorm => Format::Bc(BcKind::Bc7),
        P::Bc7RgbaUnormSrgb => Format::Bc(BcKind::Bc7Srgb),
        // The index into Metal's ASTC block sizes: 4x4 0, 5x5 2, 6x6 4, 8x8 7.
        P::Astc4x4Ldr => astc(0, false),
        P::Astc4x4Srgb => astc(0, true),
        P::Astc5x5Ldr => astc(2, false),
        P::Astc5x5Srgb => astc(2, true),
        P::Astc6x6Ldr => astc(4, false),
        P::Astc6x6Srgb => astc(4, true),
        P::Astc8x8Ldr => astc(7, false),
        P::Astc8x8Srgb => astc(7, true),
    })
}

/// The Metal format of a `VkFormat` that a texture can have.
pub(crate) fn texture_format(vk: u32) -> Option<Format> {
    nxkit_vk::format::texture_format(vk)
        .and_then(|f| f.pixel)
        .and_then(pixel)
}

/// The Metal vertex format. None for `E5B9G9R9_UFLOAT_PACK32`, which `nxkit-metal` has no vertex format for.
pub(crate) fn vertex(v: V) -> Option<MV> {
    Some(match v {
        V::UChar => MV::UChar,
        V::UChar2 => MV::UChar2,
        V::UChar3 => MV::UChar3,
        V::UChar4 => MV::UChar4,
        V::Char => MV::Char,
        V::Char2 => MV::Char2,
        V::Char3 => MV::Char3,
        V::Char4 => MV::Char4,
        V::UCharNormalized => MV::UCharNorm,
        V::UChar2Normalized => MV::UChar2Norm,
        V::UChar3Normalized => MV::UChar3Norm,
        V::UChar4Normalized => MV::UChar4Norm,
        V::UChar4NormalizedBgra => MV::UChar4NormBgra,
        V::CharNormalized => MV::CharNorm,
        V::Char2Normalized => MV::Char2Norm,
        V::Char3Normalized => MV::Char3Norm,
        V::Char4Normalized => MV::Char4Norm,
        V::UShort => MV::UShort,
        V::UShort2 => MV::UShort2,
        V::UShort3 => MV::UShort3,
        V::UShort4 => MV::UShort4,
        V::Short => MV::Short,
        V::Short2 => MV::Short2,
        V::Short3 => MV::Short3,
        V::Short4 => MV::Short4,
        V::UShortNormalized => MV::UShortNorm,
        V::UShort2Normalized => MV::UShort2Norm,
        V::UShort3Normalized => MV::UShort3Norm,
        V::UShort4Normalized => MV::UShort4Norm,
        V::ShortNormalized => MV::ShortNorm,
        V::Short2Normalized => MV::Short2Norm,
        V::Short3Normalized => MV::Short3Norm,
        V::Short4Normalized => MV::Short4Norm,
        V::Half => MV::Half,
        V::Half2 => MV::Half2,
        V::Half3 => MV::Half3,
        V::Half4 => MV::Half4,
        V::UInt => MV::UInt,
        V::UInt2 => MV::UInt2,
        V::UInt3 => MV::UInt3,
        V::UInt4 => MV::UInt4,
        V::Int => MV::Int,
        V::Int2 => MV::Int2,
        V::Int3 => MV::Int3,
        V::Int4 => MV::Int4,
        V::Float => MV::Float,
        V::Float2 => MV::Float2,
        V::Float3 => MV::Float3,
        V::Float4 => MV::Float4,
        V::UInt1010102Normalized => MV::UInt1010102Norm,
        V::FloatRg11b10 => MV::FloatRg11B10,
        V::FloatRgb9e5 => return None,
    })
}

/// The shape of a texture of `nxkit-metal` and its array length for a kind of `nxkit-vk` with `layers` Vulkan
/// layers (6 per cube). A multisample kind is its 2D kind: the sample count makes it multisample
/// (`Texture::new_multisample`). None for a buffer texture, which `nxkit-metal` cannot make.
pub(crate) fn kind(k: TextureKind, layers: u32) -> Option<(nxkit_metal::TextureKind, u32)> {
    use nxkit_metal::TextureKind as M;
    Some(match k {
        TextureKind::D2 | TextureKind::D2Multisample => (M::D2, 1),
        TextureKind::D2Array | TextureKind::D2MultisampleArray => (M::D2Array, layers.max(1)),
        TextureKind::Cube => (M::Cube, 1),
        TextureKind::CubeArray => (M::CubeArray, (layers / 6).max(1)),
        TextureKind::D3 => (M::D3, 1),
        TextureKind::Buffer => return None,
    })
}

/// The first slice and the slice count of a view in the units of `nxkit_metal::ViewDesc`: cubes for a cube array,
/// layers otherwise, and the one slice of a 3D texture.
pub(crate) fn view_slices(k: nxkit_metal::TextureKind, base_layer: u32, layers: u32) -> (u32, u32) {
    use nxkit_metal::TextureKind as M;
    match k {
        M::CubeArray => (base_layer / 6, (layers / 6).max(1)),
        M::Cube => (base_layer, 6),
        M::D3 => (0, 1),
        M::D2 => (base_layer, 1),
        M::D2Array => (base_layer, layers.max(1)),
    }
}

/// The channel selector of `Texture::view_swizzled`: 0 zero, 1 one, 2 red, 3 green, 4 blue, 5 alpha.
pub(crate) fn swizzle(s: [Swizzle; 4]) -> [u8; 4] {
    s.map(|c| match c {
        Swizzle::Zero => 0,
        Swizzle::One => 1,
        Swizzle::R => 2,
        Swizzle::G => 3,
        Swizzle::B => 4,
        Swizzle::A => 5,
    })
}

fn wrap(mode: u32) -> Wrap {
    match mode {
        0 => Wrap::Repeat,
        1 => Wrap::MirrorRepeat,
        3 => Wrap::ClampToBorder,
        4 => Wrap::MirrorClampToEdge,
        _ => Wrap::ClampToEdge,
    }
}

fn filter(f: u32) -> Filter {
    if f == 1 {
        Filter::Linear
    } else {
        Filter::Nearest
    }
}

/// The sampler of `nxkit-metal` for a Vulkan sampler. Unnormalized coordinates are not part of it (the caller
/// reports them).
pub(crate) fn sampler(d: &SamplerDesc) -> nxkit_metal::SamplerDesc {
    nxkit_metal::SamplerDesc {
        min: filter(d.min_filter),
        mag: filter(d.mag_filter),
        // Vulkan always has mip levels; `maxLod` 0.25 is how a game asks for none.
        mip: if d.mipmap_mode == 1 {
            MipFilter::Linear
        } else {
            MipFilter::Nearest
        },
        wrap: d.address.map(wrap),
        // VkBorderColor: 0 and 1 transparent black, 2 and 3 opaque black, 4 and 5 opaque white.
        border: match d.border_color {
            4.. => Border::OpaqueWhite,
            2 | 3 => Border::OpaqueBlack,
            _ => Border::TransparentBlack,
        },
        max_anisotropy: (d.max_anisotropy.round() as u32).clamp(1, 16),
        compare: d.compare.map(compare),
        reduction: nxkit_metal::Reduction::Average,
        lod_min: d.min_lod,
        lod_max: d.max_lod,
        lod_bias: d.lod_bias.clamp(-15.0, 15.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_texture_format_of_the_table_has_a_metal_format() {
        for f in nxkit_vk::format::FORMATS {
            if let Some(p) = f.pixel {
                assert!(pixel(p).is_some(), "VkFormat {}", f.vk);
            }
        }
        assert_eq!(texture_format(37), Some(Format::Rgba8Unorm));
        assert_eq!(texture_format(44), Some(Format::Bgra8Unorm));
        assert_eq!(texture_format(130), Some(Format::Depth32FloatStencil8));
        assert_eq!(texture_format(133), Some(Format::Bc(BcKind::Bc1)));
        assert_eq!(
            texture_format(171),
            Some(Format::Astc {
                size: 7,
                srgb: false
            })
        );
        assert_eq!(pixel(P::X32Stencil8), None);
        // Packed formats keep their size: B5G6R5 is 2 bytes in both tables.
        assert_eq!(texture_format(4).map(Format::bytes_per_pixel), Some(2));
    }

    #[test]
    fn vertex_formats_map_by_meaning() {
        let all: Vec<_> = nxkit_vk::format::FORMATS
            .iter()
            .filter_map(|f| f.vertex.map(|v| (f.vk, v)))
            .collect();
        let missing: Vec<u32> = all
            .iter()
            .filter(|(_, v)| vertex(*v).is_none())
            .map(|(k, _)| *k)
            .collect();
        assert_eq!(missing, vec![123], "only E5B9G9R9 has no vertex format");
        assert_eq!(vertex(V::UChar4NormalizedBgra), Some(MV::UChar4NormBgra));
        assert_eq!(vertex(V::Short3Normalized), Some(MV::Short3Norm));
        assert_eq!(vertex(V::Float3), Some(MV::Float3));
    }

    #[test]
    fn kinds_count_cubes_and_refuse_multisample() {
        use nxkit_metal::TextureKind as M;
        assert_eq!(kind(TextureKind::D2Array, 4), Some((M::D2Array, 4)));
        assert_eq!(kind(TextureKind::CubeArray, 12), Some((M::CubeArray, 2)));
        assert_eq!(kind(TextureKind::Cube, 6), Some((M::Cube, 1)));
        assert_eq!(kind(TextureKind::D2Multisample, 1), Some((M::D2, 1)));
        assert_eq!(
            kind(TextureKind::D2MultisampleArray, 3),
            Some((M::D2Array, 3))
        );
        assert_eq!(kind(TextureKind::Buffer, 1), None);
        assert_eq!(view_slices(M::CubeArray, 6, 12), (1, 2));
        assert_eq!(view_slices(M::D2, 3, 1), (3, 1));
        assert_eq!(view_slices(M::D3, 5, 1), (0, 1));
    }

    #[test]
    fn swizzles_and_samplers() {
        assert_eq!(
            swizzle([Swizzle::R, Swizzle::G, Swizzle::B, Swizzle::One]),
            [2, 3, 4, 1]
        );
        let s = sampler(&SamplerDesc {
            mag_filter: 1,
            min_filter: 0,
            mipmap_mode: 1,
            address: [0, 2, 3],
            lod_bias: 20.0,
            max_anisotropy: 8.0,
            compare: Some(3),
            min_lod: 0.0,
            max_lod: 4.0,
            border_color: 4,
            unnormalized: false,
        });
        assert_eq!(
            (s.mag, s.min, s.mip),
            (Filter::Linear, Filter::Nearest, MipFilter::Linear)
        );
        assert_eq!(
            s.wrap,
            [Wrap::Repeat, Wrap::ClampToEdge, Wrap::ClampToBorder]
        );
        assert_eq!(s.border, Border::OpaqueWhite);
        assert_eq!(s.lod_bias, 15.0);
        assert_eq!(s.max_anisotropy, 8);
        assert_eq!(s.compare, Some(nxkit_metal::CompareFn::LessEqual));
    }
}
