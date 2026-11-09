//! NVN state values to `nxkit-metal` state.
//!
//! Every function takes the raw `NVN*` enum value and returns the Metal side, or `None` when Metal has no equivalent.
//! The tables follow `NVN_*` in `nvn.h` of the NVN headers.
//! The behaviour for the values that Metal lacks (fan, quads, byte indices) is the caller's decision.

use nxkit_metal::{
    BlendFactor, BlendOp, BlendState, Border, ColorTarget, CompareFn, CullMode, DepthStencilDesc,
    FillMode, Filter, Format, IndexType, MipFilter, Primitive, Reduction, SamplerDesc, StencilFace,
    StencilOp, StepFunction, TopologyClass, VertexAttribute, VertexDescriptor, VertexFormat,
    VertexLayout, Winding, Wrap,
};
use nxkit_nvn::tic::Tsc;

use crate::detail::{BlendDetail, DepthStencilDetail, DrawDetail, StencilDetail};
use crate::format_map::{is_integer, vertex_format};

/// The Metal buffer slot of vertex stream `stream`. The streams use the slots from 16 so that they do not meet the
/// argument buffer at slot 0 and the draw support buffers.
pub(crate) fn stream_slot(stream: u32) -> u32 {
    if stream < 14 { 16 + stream } else { stream }
}

/// A constant stream for shader inputs with no enabled, bound vertex attribute.
pub(crate) const DEFAULT_VERTEX_SLOT: u32 = 30;

pub(crate) fn needs_default_vertex(d: &DrawDetail, inputs: &[(u32, VertexFormat)]) -> bool {
    inputs.iter().any(|(location, _)| {
        !d.attributes
            .iter()
            .any(|a| a.location == *location && d.streams.iter().any(|s| s.index == a.stream))
    })
}

/// `NVNdrawPrimitive` to the Metal primitive and its topology class.
pub(crate) fn primitive(nvn: u32) -> Option<(Primitive, TopologyClass)> {
    Some(match nvn {
        0 => (Primitive::Point, TopologyClass::Point),
        1 => (Primitive::Line, TopologyClass::Line),
        3 => (Primitive::LineStrip, TopologyClass::Line),
        4 => (Primitive::Triangle, TopologyClass::Triangle),
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

/// `NVNblendFunc`. Unknown values give `One`.
pub(crate) fn blend_factor(nvn: u32) -> BlendFactor {
    match nvn {
        1 => BlendFactor::Zero,
        3 => BlendFactor::SrcColor,
        4 => BlendFactor::OneMinusSrcColor,
        5 => BlendFactor::SrcAlpha,
        6 => BlendFactor::OneMinusSrcAlpha,
        7 => BlendFactor::DstAlpha,
        8 => BlendFactor::OneMinusDstAlpha,
        9 => BlendFactor::DstColor,
        10 => BlendFactor::OneMinusDstColor,
        11 => BlendFactor::SrcAlphaSaturated,
        0x10 => BlendFactor::Src1Color,
        0x11 => BlendFactor::OneMinusSrc1Color,
        0x12 => BlendFactor::Src1Alpha,
        0x13 => BlendFactor::OneMinusSrc1Alpha,
        0x61 => BlendFactor::BlendColor,
        0x62 => BlendFactor::OneMinusBlendColor,
        0x63 => BlendFactor::BlendAlpha,
        0x64 => BlendFactor::OneMinusBlendAlpha,
        _ => BlendFactor::One,
    }
}

/// `NVNblendEquation`. Unknown values give `Add`.
pub(crate) fn blend_op(nvn: u32) -> BlendOp {
    match nvn {
        2 => BlendOp::Subtract,
        3 => BlendOp::ReverseSubtract,
        4 => BlendOp::Min,
        5 => BlendOp::Max,
        _ => BlendOp::Add,
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

/// The colour target of a draw for a target of `format`. Integer formats never blend. `written` is false when the
/// shader does not write the output, and then the mask is zero.
pub(crate) fn color_target(
    format: Format,
    blend: &BlendDetail,
    mask: u8,
    written: bool,
) -> ColorTarget {
    let mut t = ColorTarget::new(format);
    t.write_mask = if written { mask & 0xF } else { 0 };
    if blend.enable && !is_integer(format) {
        t.blend = Some(BlendState {
            src_rgb: blend_factor(blend.src_rgb),
            dst_rgb: blend_factor(blend.dst_rgb),
            op_rgb: blend_op(blend.op_rgb),
            src_alpha: blend_factor(blend.src_alpha),
            dst_alpha: blend_factor(blend.dst_alpha),
            op_alpha: blend_op(blend.op_alpha),
        });
    }
    t
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

/// The blend word of a draw for the pixel shader `fs`: 3 bits per colour target, the kind of its source RGB factor
/// (see [`blend_zero_kind`]). The shader writes RGB 0 for a pixel whose factor is exactly 0, because the console's
/// blender gives 0 for `0 x NaN` and Metal's gives NaN. The word is 0 for the knob `nobz`.
pub(crate) fn blend_zero_word(
    d: &DrawDetail,
    fs: &crate::programs::StageProgram,
    targets: &crate::pipelines::Targets,
) -> u32 {
    blend_zero_bits(
        &d.blend,
        &targets.colors,
        fs.colors.as_deref(),
        crate::knob("nobz"),
    )
}

/// [`blend_zero_word`] over its parts. `written` lists the colours that the shader writes, `None` when it is not
/// known (all of them). A target has kind 0 unless it blends, has a float format, is written and has an equation
/// other than MIN and MAX. The kinds 4 and 5 (the second source) are never set: the host runs no dual source shader.
fn blend_zero_bits(
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
        if !b.enable || is_integer(format) || !is_written || matches!(b.op_rgb, 4 | 5) {
            continue;
        }
        word |= blend_zero_kind(b.src_rgb) << (3 * i);
    }
    word
}

/// `format` with the scalar type that the shader reads (`read` is `Float4`, `UInt4` or `Int4`).
///
/// The vertex input registers of Maxwell have no type: an attribute of 32-bit floats that the shader reads as
/// integers hands over the bits of the floats. Metal refuses a pipeline whose attribute format and shader type differ
/// in kind, so a 32-bit format takes the kind of the shader, which keeps the bits. The other formats convert their
/// values on fetch and have no bit-exact equivalent; they stay as they are.
pub(crate) fn as_read(format: VertexFormat, read: VertexFormat) -> VertexFormat {
    use VertexFormat as V;
    let width = match format {
        V::Float | V::UInt | V::Int => 1,
        V::Float2 | V::UInt2 | V::Int2 => 2,
        V::Float3 | V::UInt3 | V::Int3 => 3,
        V::Float4 | V::UInt4 | V::Int4 => 4,
        _ => return format,
    };
    let row = match read {
        V::UInt4 => [V::UInt, V::UInt2, V::UInt3, V::UInt4],
        V::Int4 => [V::Int, V::Int2, V::Int3, V::Int4],
        _ => [V::Float, V::Float2, V::Float3, V::Float4],
    };
    row[width - 1]
}

/// Why a vertex layout could not be translated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayoutError {
    /// The `NVNformat` of an attribute has no Metal vertex format.
    Format(u32),
}

/// The vertex descriptor of a draw. A stream with stride 0 steps as a constant. A stream with a divisor steps per
/// instance. Shader inputs without a bound attribute read a constant zero stream. Metal requires a descriptor
/// even for those inputs; omitting one makes the entire pipeline fail to compile.
pub(crate) fn vertex_descriptor(
    d: &DrawDetail,
    inputs: &[(u32, VertexFormat)],
) -> Result<VertexDescriptor, LayoutError> {
    let mut out = VertexDescriptor::default();
    for a in &d.attributes {
        if !d.streams.iter().any(|s| s.index == a.stream) {
            continue;
        }
        let mut format = vertex_format(a.format).ok_or(LayoutError::Format(a.format))?;
        if let Some((_, read)) = inputs.iter().find(|(l, _)| *l == a.location) {
            format = as_read(format, *read);
        }
        out.attributes.push((
            a.location,
            VertexAttribute {
                format,
                offset: a.offset,
                buffer: stream_slot(a.stream),
            },
        ));
    }
    for (location, format) in inputs {
        if !out.attributes.iter().any(|(l, _)| l == location) {
            out.attributes.push((
                *location,
                VertexAttribute {
                    format: *format,
                    offset: 0,
                    buffer: DEFAULT_VERTEX_SLOT,
                },
            ));
        }
    }
    out.attributes.sort_by_key(|(l, _)| *l);
    for s in &d.streams {
        if !d.attributes.iter().any(|a| a.stream == s.index) {
            continue;
        }
        let (step, step_rate) = if s.stride == 0 {
            (StepFunction::Constant, 0)
        } else if s.divisor != 0 {
            (StepFunction::PerInstance, s.divisor)
        } else {
            (StepFunction::PerVertex, 1)
        };
        // Metal refuses a layout with stride 0, also for a constant step that never moves (an assertion in the archive,
        // "references a buffer ... that has no stride"; Ocarina of Time after its opening). The stride of a constant
        // step is not read, so any value that covers the attributes does: their furthest end, at least 16, in 4s.
        let stride = if s.stride == 0 {
            let end = d
                .attributes
                .iter()
                .filter(|a| a.stream == s.index)
                .map(|a| a.offset + 16)
                .max()
                .unwrap_or(16);
            end.max(16).next_multiple_of(4)
        } else {
            s.stride
        };
        out.layouts.push(VertexLayout {
            buffer: stream_slot(s.index),
            stride,
            step,
            step_rate,
        });
    }
    if needs_default_vertex(d, inputs) {
        out.layouts.push(VertexLayout {
            buffer: DEFAULT_VERTEX_SLOT,
            stride: 16,
            step: StepFunction::Constant,
            step_rate: 0,
        });
    }
    out.layouts.sort_by_key(|l| l.buffer);
    Ok(out)
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
        assert!(primitive(7).is_none());
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

    #[test]
    fn the_blend_word_is_zero_without_blending() {
        let off = BlendDetail::default();
        assert_eq!(blend_zero_bits(&[off, off], &[F16, F16], None, false), 0);
        assert_eq!(blend_zero_bits(&[], &[F16], None, false), 0);
    }

    #[test]
    fn the_blend_word_has_three_bits_per_target() {
        let off = BlendDetail::default();
        let b = [off, blend(5), blend(6), blend(1)];
        let colors = [F16, F16, F16, F16];
        assert_eq!(
            blend_zero_bits(&b, &colors, None, false),
            (2 << 3) | (3 << 6) | (1 << 9)
        );
        // The lone SRC_ALPHA on target 1.
        assert_eq!(
            blend_zero_bits(&[off, blend(5)], &[F16, F16], None, false),
            2 << 3
        );
    }

    #[test]
    fn the_blend_word_skips_what_the_rule_does_not_cover() {
        let int = Some(Format::Rgba8Uint);
        assert_eq!(blend_zero_bits(&[blend(5)], &[int], None, false), 0);
        assert_eq!(blend_zero_bits(&[blend(5)], &[None], None, false), 0);
        for op in [4, 5] {
            let b = BlendDetail {
                op_rgb: op,
                ..blend(5)
            };
            assert_eq!(blend_zero_bits(&[b], &[F16], None, false), 0, "op {op}");
        }
        // SRC1_ALPHA, DST_ALPHA, ONE, BLEND_COLOR and SRC_ALPHA_SATURATE.
        for f in [0x12, 0x13, 7, 2, 0x61, 11, 0] {
            assert_eq!(
                blend_zero_bits(&[blend(f)], &[F16], None, false),
                0,
                "factor {f}"
            );
        }
        let b = [blend(5), blend(5)];
        // A target that the shader does not write.
        assert_eq!(blend_zero_bits(&b, &[F16, F16], Some(&[0]), false), 2);
        assert_eq!(blend_zero_bits(&b, &[F16, F16], Some(&[1]), false), 2 << 3);
    }

    #[test]
    fn the_knob_nobz_gives_the_word_zero() {
        assert_eq!(blend_zero_bits(&[blend(5)], &[F16], None, false), 2);
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
