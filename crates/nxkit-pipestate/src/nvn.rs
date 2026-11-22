//! The NVN values of a draw's pipeline state and the state they map to.
//!
//! The NVN values come from `nvn.h` (see `THIRD_PARTY.md`). The host translates a draw with these functions
//! (`nxkit-gpu`), and the build resolves a seed with them (`crate::seeds`), so both arrive at the same pipeline.

use crate::format::{BcKind, Format};
use crate::state::{
    BlendFactor, BlendOp, BlendState, ColorTarget, StepFunction, TopologyClass, VertexAttribute,
    VertexDescriptor, VertexFormat, VertexLayout,
};

/// The blend state of one colour target (`NVNblendFunc` and `NVNblendEquation` values).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Blend {
    /// Whether blending is on.
    pub enable: bool,
    /// The source factor of RGB.
    pub src_rgb: u32,
    /// The destination factor of RGB.
    pub dst_rgb: u32,
    /// The equation of RGB.
    pub op_rgb: u32,
    /// The source factor of alpha.
    pub src_alpha: u32,
    /// The destination factor of alpha.
    pub dst_alpha: u32,
    /// The equation of alpha.
    pub op_alpha: u32,
}

/// One vertex attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Attribute {
    /// The attribute index. It is the `location` of the vertex input of the shader.
    pub location: u32,
    /// The `NVNformat` value of the attribute.
    pub format: u32,
    /// The byte offset in the vertex.
    pub offset: u32,
    /// The stream (vertex buffer binding) the attribute reads.
    pub stream: u32,
}

/// What the vertex layout needs of a vertex stream.
pub trait StreamState {
    /// The stream index.
    fn index(&self) -> u32;
    /// The bytes between two vertices. 0 means one value for every vertex.
    fn stride(&self) -> u32;
    /// The instance divisor. 0 means per vertex.
    fn divisor(&self) -> u32;
}

/// A vertex stream without its buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stream {
    /// The stream index.
    pub index: u32,
    /// The bytes between two vertices. 0 means one value for every vertex.
    pub stride: u32,
    /// The instance divisor. 0 means per vertex.
    pub divisor: u32,
}

impl StreamState for Stream {
    fn index(&self) -> u32 {
        self.index
    }
    fn stride(&self) -> u32 {
        self.stride
    }
    fn divisor(&self) -> u32 {
        self.divisor
    }
}

/// How a texture is used by the frame. It decides the Metal format and the usage flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapped {
    /// A format with the same bytes per block as NVN.
    Exact(Format),
    /// A format with a different layout from NVN (depth 24 bits). The texture is a render target only. It is never
    /// deswizzled from guest memory.
    Widened(Format),
}

impl Mapped {
    /// The format.
    pub fn format(self) -> Format {
        match self {
            Self::Exact(f) | Self::Widened(f) => f,
        }
    }
}

/// The format of an NVN texture format, or `None` when Metal has no equivalent.
///
/// Three-channel formats (0x19 to 0x24) have no Metal pixel format. The UI2F formats and the packed 5-5-5-1 and 4-4-4-4
/// variants beyond the rows below are not mapped.
pub fn texture_format(nvn: i32) -> Option<Mapped> {
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
        0x41 | 0x42 | 0x45 | 0x46 => exact(F::Bc(if matches!(nvn, 0x45 | 0x46) {
            BcKind::Bc1Srgb
        } else {
            BcKind::Bc1
        })),
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
        // RGBX: the bytes of RGBA, and a read of alpha gives one ([`sample_swizzle`]). 0x6D is checked against the
        // 16x16x16 colour grading table of Xenoblade Chronicles 3, the other rows are unverified.
        0x6D => exact(F::Rgba8Unorm),
        0x6E => exact(F::Rgba8Snorm),
        0x6F => exact(F::Rgba8Uint),
        0x70 => exact(F::Rgba8Sint),
        0x71 => exact(F::Rgba16Float),
        0x72 => exact(F::Rgba16Unorm),
        0x73 => exact(F::Rgba16Snorm),
        0x74 => exact(F::Rgba16Uint),
        0x75 => exact(F::Rgba16Sint),
        0x76 => exact(F::Rgba32Float),
        0x77 => exact(F::Rgba32Uint),
        0x78 => exact(F::Rgba32Sint),
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

/// The channel swizzle of a sampling view of NVN format `nvn` (0 zero, 1 one, 2 R, 3 G, 4 B, 5 A). An RGBX format
/// (0x6D to 0x78) has no alpha in its texels, so a read of alpha gives one.
pub fn sample_swizzle(nvn: i32, swizzle: [u8; 4]) -> [u8; 4] {
    if (0x6D..=0x78).contains(&nvn) {
        swizzle.map(|s| if s == 5 { 1 } else { s })
    } else {
        swizzle
    }
}

/// True for the formats that Metal cannot blend or that have integer channels. The pipeline never blends them.
pub fn is_integer(f: Format) -> bool {
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

/// The vertex format of an NVN vertex attribute format (`vfmt`).
///
/// The one, two, three and four component groups start at 0x01, 0x0D, 0x19 and 0x25. The special formats are
/// 0x3D UInt1010102Norm, 0x69 Int1010102Norm, 0x3F FloatRG11B10 and 0x9A UChar4Norm BGRA.
pub fn vertex_format(nvn: u32) -> Option<VertexFormat> {
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
        [
            V::UShortNorm,
            V::UShort2Norm,
            V::UShort3Norm,
            V::UShort4Norm,
        ],
        [V::ShortNorm, V::Short2Norm, V::Short3Norm, V::Short4Norm],
        [V::UShort, V::UShort2, V::UShort3, V::UShort4],
        [V::Short, V::Short2, V::Short3, V::Short4],
        [V::Float, V::Float2, V::Float3, V::Float4],
        [V::UInt, V::UInt2, V::UInt3, V::UInt4],
        [V::Int, V::Int2, V::Int3, V::Int4],
    ];
    TABLE.get(t as usize)?.get(n as usize).copied()
}

/// `NVN_DRAW_PRIMITIVE_QUADS`.
pub const QUADS: u32 = 7;

/// The topology class of an `NVNdrawPrimitive`. Quads draw as triangles.
pub fn topology_class(nvn: u32) -> Option<TopologyClass> {
    Some(match nvn {
        0 => TopologyClass::Point,
        1 | 3 => TopologyClass::Line,
        4 | 5 | QUADS => TopologyClass::Triangle,
        _ => return None,
    })
}

/// `NVNblendFunc`. Unknown values give `One`.
pub fn blend_factor(nvn: u32) -> BlendFactor {
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
pub fn blend_op(nvn: u32) -> BlendOp {
    match nvn {
        2 => BlendOp::Subtract,
        3 => BlendOp::ReverseSubtract,
        4 => BlendOp::Min,
        5 => BlendOp::Max,
        _ => BlendOp::Add,
    }
}

/// The colour target of a draw for a target of `format`. Integer formats never blend. `written` is false when the
/// shader does not write the output, and then the mask is zero.
pub fn color_target(format: Format, blend: &Blend, mask: u8, written: bool) -> ColorTarget {
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

/// The Metal buffer slot of vertex stream `stream`. The streams use the slots from 16 so that they do not meet the
/// argument buffer at slot 0 and the draw support buffers.
pub fn stream_slot(stream: u32) -> u32 {
    if stream < 14 { 16 + stream } else { stream }
}

/// A constant stream for shader inputs with no enabled, bound vertex attribute.
pub const DEFAULT_VERTEX_SLOT: u32 = 30;

/// True when a shader input has no attribute of a bound stream, so the draw needs the constant default vertex.
pub fn needs_default_vertex<S: StreamState>(
    attributes: &[Attribute],
    streams: &[S],
    inputs: &[(u32, VertexFormat)],
) -> bool {
    inputs.iter().any(|(location, _)| {
        !attributes
            .iter()
            .any(|a| a.location == *location && streams.iter().any(|s| s.index() == a.stream))
    })
}

/// `format` with the scalar type that the shader reads (`read` is `Float4`, `UInt4` or `Int4`).
///
/// The vertex input registers of Maxwell have no type: an attribute of 32-bit floats that the shader reads as
/// integers hands over the bits of the floats. Metal refuses a pipeline whose attribute format and shader type differ
/// in kind, so a 32-bit format takes the kind of the shader, which keeps the bits. The other formats convert their
/// values on fetch and have no bit-exact equivalent; they stay as they are.
pub fn as_read(format: VertexFormat, read: VertexFormat) -> VertexFormat {
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
pub enum LayoutError {
    /// The `NVNformat` of an attribute has no Metal vertex format.
    Format(u32),
}

/// The vertex descriptor of a draw. A stream with stride 0 steps as a constant. A stream with a divisor steps per
/// instance. Shader inputs without a bound attribute read a constant zero stream. Metal requires a descriptor
/// even for those inputs; omitting one makes the entire pipeline fail to compile.
///
/// Only the attributes that the shader reads (`inputs`) and their streams are in the descriptor. The others change
/// nothing that the pipeline draws, and leaving them out gives two draws that differ only in them one pipeline. It
/// also makes the layout of a seed (`crate::seeds`), which holds only the attributes the shader reads, the layout of
/// the draw.
pub fn vertex_descriptor<S: StreamState>(
    attributes: &[Attribute],
    streams: &[S],
    inputs: &[(u32, VertexFormat)],
) -> Result<VertexDescriptor, LayoutError> {
    let read: Vec<&Attribute> = attributes
        .iter()
        .filter(|a| inputs.iter().any(|(l, _)| *l == a.location))
        .collect();
    let mut out = VertexDescriptor::default();
    for a in &read {
        if !streams.iter().any(|s| s.index() == a.stream) {
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
    for s in streams {
        if !read.iter().any(|a| a.stream == s.index()) {
            continue;
        }
        let (step, step_rate) = if s.stride() == 0 {
            (StepFunction::Constant, 0)
        } else if s.divisor() != 0 {
            (StepFunction::PerInstance, s.divisor())
        } else {
            (StepFunction::PerVertex, 1)
        };
        // Metal refuses a layout with stride 0, also for a constant step that never moves (an assertion in the archive,
        // "references a buffer ... that has no stride"; Ocarina of Time after its opening). The stride of a constant
        // step is not read, so any value that covers the attributes does: their furthest end, at least 16, in 4s.
        let stride = if s.stride() == 0 {
            let end = read
                .iter()
                .filter(|a| a.stream == s.index())
                .map(|a| a.offset + 16)
                .max()
                .unwrap_or(16);
            end.max(16).next_multiple_of(4)
        } else {
            s.stride()
        };
        out.layouts.push(VertexLayout {
            buffer: stream_slot(s.index()),
            stride,
            step,
            step_rate,
        });
    }
    if needs_default_vertex(attributes, streams, inputs) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_and_depth_formats_map() {
        assert_eq!(
            texture_format(0x25).map(Mapped::format),
            Some(Format::Rgba8Unorm)
        );
        assert_eq!(
            texture_format(0x35),
            Some(Mapped::Widened(Format::Depth32FloatStencil8))
        );
        assert_eq!(texture_format(0x19), None);
        assert_eq!(
            texture_format(0x80).map(Mapped::format),
            Some(Format::Astc {
                size: 7,
                srgb: false
            })
        );
    }

    #[test]
    fn rgbx_formats_are_rgba_and_read_alpha_as_one() {
        // Xenoblade Chronicles 3 samples a 16x16x16 RGBX8 colour grading table
        assert_eq!(
            texture_format(0x6D).map(Mapped::format),
            Some(Format::Rgba8Unorm)
        );
        assert_eq!(
            texture_format(0x76).map(Mapped::format),
            Some(Format::Rgba32Float)
        );
        assert_eq!(sample_swizzle(0x6D, [2, 3, 4, 5]), [2, 3, 4, 1]);
        assert_eq!(sample_swizzle(0x6D, [5, 5, 5, 5]), [1, 1, 1, 1]);
        assert_eq!(sample_swizzle(0x25, [2, 3, 4, 5]), [2, 3, 4, 5]);
    }

    #[test]
    fn the_layout_holds_only_the_attributes_the_shader_reads() {
        let attributes = [
            Attribute {
                location: 0,
                format: 0x25 + 9,
                offset: 0,
                stream: 0,
            },
            Attribute {
                location: 1,
                format: 0x25 + 9,
                offset: 16,
                stream: 0,
            },
            Attribute {
                location: 2,
                format: 0x0D + 9,
                offset: 0,
                stream: 1,
            },
        ];
        let streams = [
            Stream {
                index: 0,
                stride: 32,
                divisor: 0,
            },
            Stream {
                index: 1,
                stride: 8,
                divisor: 0,
            },
        ];
        let d = vertex_descriptor(&attributes, &streams, &[(0, VertexFormat::Float4)]).unwrap();
        assert_eq!(d.attributes.len(), 1);
        assert_eq!(d.attributes[0].0, 0);
        assert_eq!(
            d.layouts.len(),
            1,
            "stream 1 has no attribute the shader reads"
        );
        assert_eq!(d.layouts[0].buffer, stream_slot(0));
        assert_eq!(
            d,
            vertex_descriptor(
                &attributes[..1],
                &streams[..1],
                &[(0, VertexFormat::Float4)]
            )
            .unwrap(),
            "the attributes the shader does not read change nothing"
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
