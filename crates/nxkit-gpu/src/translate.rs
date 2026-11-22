//! NVN state values to `nxkit-metal` state. The values of a pipeline's state are `nxkit-pipestate`'s tables (`nvn`),
//! shared with the build. This module holds the rest (depth, stencil, raster, samplers) and the draw's adapters.
//!
//! Every function takes the raw `NVN*` enum value and returns the Metal side, or `None` when Metal has no equivalent.
//! The tables follow `NVN_*` in `nvn.h` of the NVN headers.
//! The behaviour for the values that Metal lacks (fan, quads, byte indices) is the caller's decision.

use nxkit_metal::{
    Border, CompareFn, CullMode, DepthStencilDesc, FillMode, Filter, Format, IndexType, MipFilter,
    Primitive, Reduction, SamplerDesc, StencilFace, StencilOp, TopologyClass, VertexDescriptor,
    VertexFormat, Winding, Wrap,
};
use nxkit_nvn::tic::Tsc;

use nxkit_pipestate::nvn;
pub(crate) use nxkit_pipestate::nvn::{DEFAULT_VERTEX_SLOT, LayoutError, QUADS, stream_slot};

use crate::detail::{BlendDetail, DepthStencilDetail, DrawDetail, StencilDetail};
use crate::format_map::is_integer;

/// `NVNdrawPrimitive` to the Metal primitive and its topology class. Quads draw as triangles, and
/// `crate::geometry::geometry` writes the indices of two triangles for each quad.
pub(crate) fn primitive(nvn: u32) -> Option<(Primitive, TopologyClass)> {
    Some(match nvn {
        0 => (Primitive::Point, TopologyClass::Point),
        1 => (Primitive::Line, TopologyClass::Line),
        3 => (Primitive::LineStrip, TopologyClass::Line),
        4 | QUADS => (Primitive::Triangle, TopologyClass::Triangle),
        5 => (Primitive::TriangleStrip, TopologyClass::Triangle),
        _ => return None,
    })
}

/// `NVNindexType` to the Metal index type. Metal has no byte indices.
pub(crate) fn index_type(nvn: u32) -> Option<IndexType> {
    match nvn {
        1 => Some(IndexType::U16),
        2 => Some(IndexType::U32),
        _ => None,
    }
}

/// `NVNcompareFunc` and `NVNdepthFunc` (the same values).
pub(crate) fn compare(nvn: u32) -> CompareFn {
    match nvn {
        1 => CompareFn::Never,
        2 => CompareFn::Less,
        3 => CompareFn::Equal,
        4 => CompareFn::LessEqual,
        5 => CompareFn::Greater,
        6 => CompareFn::NotEqual,
        7 => CompareFn::GreaterEqual,
        _ => CompareFn::Always,
    }
}

/// `NVNstencilOp`.
pub(crate) fn stencil_op(nvn: u32) -> StencilOp {
    match nvn {
        2 => StencilOp::Zero,
        3 => StencilOp::Replace,
        4 => StencilOp::IncrementClamp,
        5 => StencilOp::DecrementClamp,
        6 => StencilOp::Invert,
        7 => StencilOp::IncrementWrap,
        8 => StencilOp::DecrementWrap,
        _ => StencilOp::Keep,
    }
}

/// `NVNface` to a Metal cull mode. Front and back together draws nothing, which the caller handles.
pub(crate) fn cull(nvn: u32) -> Option<CullMode> {
    match nvn {
        0 => Some(CullMode::None),
        1 => Some(CullMode::Front),
        2 => Some(CullMode::Back),
        _ => None,
    }
}

/// `NVNfrontFace`.
pub(crate) fn winding(nvn: u32) -> Winding {
    if nvn == 0 {
        Winding::Clockwise
    } else {
        Winding::CounterClockwise
    }
}

/// `NVNpolygonMode`. Metal has fill and lines. Points are drawn as lines.
pub(crate) fn fill(nvn: u32) -> FillMode {
    if nvn == 2 {
        FillMode::Fill
    } else {
        FillMode::Lines
    }
}

fn stencil_face(s: &StencilDetail) -> StencilFace {
    StencilFace {
        compare: compare(s.func),
        fail: stencil_op(s.fail),
        depth_fail: stencil_op(s.depth_fail),
        pass: stencil_op(s.pass),
        read_mask: u32::from(s.read_mask),
        write_mask: u32::from(s.write_mask),
    }
}

/// The depth and stencil state of a draw. A pass without a depth attachment sets the state that is off.
pub(crate) fn depth_stencil(d: &DepthStencilDetail) -> DepthStencilDesc {
    DepthStencilDesc {
        depth_compare: if d.depth_test {
            if d.depth_func == 3 && crate::knob("noeq") {
                CompareFn::Always
            } else {
                compare(d.depth_func)
            }
        } else {
            CompareFn::Always
        },
        depth_write: d.depth_write,
        front: d.stencil_test.then(|| stencil_face(&d.front)),
        back: d.stencil_test.then(|| stencil_face(&d.back)),
    }
}

/// The kind of a blend source factor in the blend word (`blendz`): 1 `ZERO`, 2 `SRC_ALPHA`, 3 `ONE_MINUS_SRC_ALPHA`.
/// Every other factor is 0: the shader does not know its value, or it is not 0 for any alpha.
fn blend_zero_kind(src_rgb: u32) -> u32 {
    match src_rgb {
        1 => 1,
        5 => 2,
        6 => 3,
        _ => 0,
    }
}

/// True when the destination RGB factor reads the source RGB: `SRC_COLOR` (3) and `ONE_MINUS_SRC_COLOR` (4). The
/// rule writes RGB 0 into the source, so every other reader of the source RGB must be absent, or the multiply blend
/// (`ZERO` / `SRC_COLOR`) would give black. The other factors of [`blend_factor`] read the source alpha (never
/// changed), the destination, the constant colour or the second source, not this source RGB.
fn dst_reads_source_rgb(dst_rgb: u32) -> bool {
    matches!(dst_rgb, 3 | 4)
}

/// `crate::drawctx::blend_zero_word` over its parts. `written` lists the colours that the shader writes, `None` when it is not
/// known (all of them). A target has kind 0 unless it blends, has a float format, is written, has an equation
/// other than MIN and MAX and its destination RGB factor does not read the source RGB ([`dst_reads_source_rgb`]).
/// The alpha blend needs no condition: its equation reads the source alpha only (`color_target` maps the alpha
/// factors to Metal's alpha channel), and the rule never changes alpha. The kinds 4 and 5 (the second source) are never set: the host runs no dual source shader.
pub(crate) fn blend_zero_bits(
    blend: &[BlendDetail],
    colors: &[Option<Format>],
    written: Option<&[u8]>,
    off: bool,
) -> u32 {
    if off {
        return 0;
    }
    let mut word = 0u32;
    for (i, format) in colors.iter().enumerate().take(8) {
        let Some(format) = *format else { continue };
        let Some(b) = blend.get(i) else { continue };
        let is_written = written.is_none_or(|c| c.contains(&(i as u8)));
        if !b.enable
            || is_integer(format)
            || !is_written
            || matches!(b.op_rgb, 4 | 5)
            || dst_reads_source_rgb(b.dst_rgb)
        {
            continue;
        }
        word |= blend_zero_kind(b.src_rgb) << (3 * i);
    }
    word
}

/// The vertex descriptor of a draw ([`nxkit_pipestate::nvn::vertex_descriptor`]).
pub(crate) fn vertex_descriptor(
    d: &DrawDetail,
    inputs: &[(u32, VertexFormat)],
) -> Result<VertexDescriptor, LayoutError> {
    nvn::vertex_descriptor(&d.attributes, &d.streams, inputs)
}

/// True when a shader input of the draw reads the constant default vertex.
pub(crate) fn needs_default_vertex(d: &DrawDetail, inputs: &[(u32, VertexFormat)]) -> bool {
    nvn::needs_default_vertex(&d.attributes, &d.streams, inputs)
}

fn wrap(nvn: u8) -> Wrap {
    match nvn {
        1 => Wrap::Repeat,
        2 | 3 => Wrap::MirrorClampToEdge,
        4 | 5 => Wrap::ClampToBorder,
        6 => Wrap::MirrorRepeat,
        _ => Wrap::ClampToEdge,
    }
}

fn border(c: [f32; 4]) -> Border {
    if c[3] < 0.5 {
        Border::TransparentBlack
    } else if c[0] + c[1] + c[2] > 1.5 {
        Border::OpaqueWhite
    } else {
        Border::OpaqueBlack
    }
}

/// The sampler of a sampler header. The filter values are `NVNminFilter` and `NVNmagFilter`.
pub(crate) fn sampler(t: &Tsc) -> SamplerDesc {
    let linear = |v: u8| {
        if v == 1 {
            Filter::Linear
        } else {
            Filter::Nearest
        }
    };
    let mip = match t.min_filter {
        2 | 3 => MipFilter::Nearest,
        4 | 5 => MipFilter::Linear,
        _ => MipFilter::None,
    };
    SamplerDesc {
        min: if t.min_filter & 1 == 1 {
            Filter::Linear
        } else {
            Filter::Nearest
        },
        mag: linear(t.mag_filter),
        mip,
        wrap: [wrap(t.wrap_s), wrap(t.wrap_t), wrap(t.wrap_r)],
        border: border(t.border),
        max_anisotropy: (t.aniso.round() as u32).clamp(1, 16),
        compare: (t.compare_mode == 1).then(|| compare(u32::from(t.compare_func))),
        reduction: match t.reduction {
            1 => Reduction::Minimum,
            2 => Reduction::Maximum,
            _ => Reduction::Average,
        },
        lod_min: t.lod_min,
        lod_max: t.lod_max,
        lod_bias: t.lod_bias,
    }
}

#[cfg(test)]
mod tests {
    use nxkit_metal::{BlendFactor, BlendOp};
    use nxkit_pipestate::nvn::{as_read, blend_factor, blend_op, color_target};

    use super::*;

    #[test]
    fn thirty_two_bit_attributes_take_the_kind_the_shader_reads() {
        use VertexFormat as V;
        assert_eq!(as_read(V::Float4, V::UInt4), V::UInt4);
        assert_eq!(as_read(V::Float2, V::Int4), V::Int2);
        assert_eq!(as_read(V::UInt3, V::Float4), V::Float3);
        assert_eq!(as_read(V::Float4, V::Float4), V::Float4);
        assert_eq!(as_read(V::UChar4Norm, V::UInt4), V::UChar4Norm);
    }

    #[test]
    fn streams_use_slots_from_16() {
        assert_eq!(stream_slot(0), 16);
        assert_eq!(stream_slot(13), 29);
        assert_eq!(stream_slot(14), 14);
    }

    #[test]
    fn primitives() {
        assert_eq!(primitive(4).map(|p| p.1), Some(TopologyClass::Triangle));
        assert!(primitive(6).is_none());
        assert_eq!(primitive(QUADS).map(|p| p.0), Some(Primitive::Triangle));
    }

    #[test]
    fn byte_indices_are_unsupported() {
        assert!(index_type(0).is_none());
        assert_eq!(index_type(2), Some(IndexType::U32));
    }

    #[test]
    fn blend_defaults() {
        assert_eq!(blend_factor(0), BlendFactor::One);
        assert_eq!(blend_factor(0x61), BlendFactor::BlendColor);
        assert_eq!(blend_op(9), BlendOp::Add);
        assert_eq!(blend_op(5), BlendOp::Max);
    }

    fn blend(src_rgb: u32) -> BlendDetail {
        BlendDetail {
            enable: true,
            src_rgb,
            dst_rgb: 6,
            ..BlendDetail::default()
        }
    }

    const F16: Option<Format> = Some(Format::Rgba16Float);

    fn word(b: &[BlendDetail], colors: &[Option<Format>]) -> u32 {
        blend_zero_bits(b, colors, None, false)
    }

    #[test]
    fn the_blend_word_is_zero_without_blending() {
        let off = BlendDetail::default();
        assert_eq!(word(&[off, off], &[F16, F16]), 0);
        assert_eq!(word(&[], &[F16]), 0);
    }

    #[test]
    fn the_blend_word_has_three_bits_per_target() {
        let off = BlendDetail::default();
        let b = [off, blend(5), blend(6), blend(1)];
        assert_eq!(word(&b, &[F16; 4]), (2 << 3) | (3 << 6) | (1 << 9));
        assert_eq!(word(&b[..2], &[F16; 2]), 2 << 3);
    }

    #[test]
    fn the_blend_word_skips_what_the_rule_does_not_cover() {
        let int = Some(Format::Rgba8Uint);
        assert_eq!(word(&[blend(5)], &[int]), 0);
        assert_eq!(word(&[blend(5)], &[None]), 0);
        for op in [4, 5] {
            let b = BlendDetail {
                op_rgb: op,
                ..blend(5)
            };
            assert_eq!(word(&[b], &[F16]), 0, "op {op}");
        }
        // SRC1_ALPHA, DST_ALPHA, ONE, BLEND_COLOR and SRC_ALPHA_SATURATE.
        for f in [0x12, 0x13, 7, 2, 0x61, 11, 0] {
            assert_eq!(word(&[blend(f)], &[F16]), 0, "factor {f}");
        }
        let b = [blend(5), blend(5)];
        // A target that the shader does not write.
        assert_eq!(blend_zero_bits(&b, &[F16, F16], Some(&[0]), false), 2);
        assert_eq!(blend_zero_bits(&b, &[F16, F16], Some(&[1]), false), 2 << 3);
    }

    #[test]
    fn a_destination_factor_that_reads_the_source_colour_gives_kind_zero() {
        let with = |src_rgb, dst_rgb| {
            [BlendDetail {
                dst_rgb,
                ..blend(src_rgb)
            }]
        };
        let w = |b: [BlendDetail; 1]| word(&b, &[F16]);
        assert_eq!((w(with(1, 3)), w(with(5, 4)), w(with(6, 3))), (0, 0, 0));
        assert_eq!((w(with(5, 6)), w(with(1, 2))), (2, 1));
    }

    #[test]
    fn the_knob_nobz_gives_the_word_zero() {
        assert_eq!(word(&[blend(5)], &[F16]), 2);
        assert_eq!(blend_zero_bits(&[blend(5)], &[F16], None, true), 0);
    }

    #[test]
    fn integer_targets_do_not_blend() {
        let b = BlendDetail {
            enable: true,
            src_rgb: 5,
            dst_rgb: 6,
            ..BlendDetail::default()
        };
        let t = color_target(Format::Rgba8Unorm, &b, 0xF, true);
        assert!(t.blend.is_some());
        let t = color_target(Format::Rgba8Uint, &b, 0xF, true);
        assert!(t.blend.is_none());
        assert_eq!(
            color_target(Format::Rgba8Unorm, &b, 0xF, false).write_mask,
            0
        );
    }

    #[test]
    fn sampler_filters() {
        let t = Tsc {
            min_filter: 5,
            mag_filter: 1,
            wrap_s: 1,
            wrap_t: 7,
            wrap_r: 5,
            compare_mode: 0,
            compare_func: 0,
            reduction: 0,
            lod_min: 0.0,
            lod_max: 12.0,
            lod_bias: 0.0,
            aniso: 4.0,
            border: [0.0, 0.0, 0.0, 1.0],
        };
        let s = sampler(&t);
        assert_eq!(
            (s.min, s.mag, s.mip),
            (Filter::Linear, Filter::Linear, MipFilter::Linear)
        );
        assert_eq!(
            s.wrap,
            [Wrap::Repeat, Wrap::ClampToEdge, Wrap::ClampToBorder]
        );
        assert_eq!(s.border, Border::OpaqueBlack);
        assert_eq!(s.max_anisotropy, 4);
    }
}
