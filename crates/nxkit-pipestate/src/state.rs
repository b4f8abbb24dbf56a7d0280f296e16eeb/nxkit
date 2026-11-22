//! Blend, topology and vertex state as plain data.
//!
//! The order of the variants and fields is part of the key scheme of the pipeline cache (`nxkit-gpu`
//! `pipelines/keys.rs`): the keys hash these values. Do not reorder them.

use crate::format::Format;

macro_rules! plain_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $v),+ }
        impl $name {
            /// Every value, in declaration order (for a text form that is read back).
            pub const ALL: &'static [Self] = &[$(Self::$v),+];
        }
    };
}

plain_enum! {
    /// A blend factor.
    BlendFactor {        /// 0.
        Zero,

        /// 1.
        One,

        /// The source colour.
        SrcColor,

        /// 1 minus the source colour.
        OneMinusSrcColor,

        /// The source alpha.
        SrcAlpha,

        /// 1 minus the source alpha.
        OneMinusSrcAlpha,

        /// The destination colour.
        DstColor,

        /// 1 minus the destination colour.
        OneMinusDstColor,

        /// The destination alpha.
        DstAlpha,

        /// 1 minus the destination alpha.
        OneMinusDstAlpha,

        /// The source alpha, saturated.
        SrcAlphaSaturated,

        /// The constant blend colour.
        BlendColor,

        /// 1 minus the constant blend colour.
        OneMinusBlendColor,

        /// The constant blend alpha.
        BlendAlpha,

        /// 1 minus the constant blend alpha.
        OneMinusBlendAlpha,

        /// The second fragment output colour (dual-source blending).
        Src1Color,

        /// 1 minus the second fragment output colour.
        OneMinusSrc1Color,

        /// The second fragment output alpha.
        Src1Alpha,

        /// 1 minus the second fragment output alpha.
        OneMinusSrc1Alpha,
    }
}

plain_enum! {
    /// A blend operation.
    BlendOp {        /// source + destination.
        Add,

        /// source - destination.
        Subtract,

        /// destination - source.
        ReverseSubtract,

        /// The smaller value.
        Min,

        /// The larger value.
        Max,
    }
}

plain_enum! {
    /// The primitive class a pipeline draws. Metal needs it when the pipeline is built.
    TopologyClass {        /// No class. Metal accepts a vertex shader that writes the point size, but not layered rendering.
        Unspecified,

        /// Points.
        Point,

        /// Lines.
        Line,

        /// Triangles.
        Triangle,
    }
}

plain_enum! {
    /// The format of one vertex attribute.
    VertexFormat {        /// One f32.
        Float,

        /// Two f32.
        Float2,

        /// Three f32.
        Float3,

        /// Four f32.
        Float4,

        /// One u32.
        UInt,

        /// Two u32.
        UInt2,

        /// Three u32.
        UInt3,

        /// Four u32.
        UInt4,

        /// One i32.
        Int,

        /// Two i32.
        Int2,

        /// Three i32.
        Int3,

        /// Four i32.
        Int4,

        /// Two f16.
        Half2,

        /// Four f16.
        Half4,

        /// Four u8, normalised.
        UChar4Norm,

        /// Four u8, normalised, in BGRA order.
        UChar4NormBgra,

        /// Four i8, normalised.
        Char4Norm,

        /// Two u16, normalised.
        UShort2Norm,

        /// Four u16, normalised.
        UShort4Norm,

        /// Two i16, normalised.
        Short2Norm,

        /// Four i16, normalised.
        Short4Norm,

        /// Four u8, not normalised.
        UChar4,

        /// Four u16, not normalised.
        UShort4,

        /// Four i16, not normalised.
        Short4,

        /// 10-10-10-2 signed, normalised.
        Int1010102Norm,

        /// 10-10-10-2 unsigned, normalised.
        UInt1010102Norm,

        /// One u8, not normalised.
        UChar,

        /// One i8, not normalised.
        Char,

        /// One u8, normalised.
        UCharNorm,

        /// One i8, normalised.
        CharNorm,

        /// Two u8, not normalised.
        UChar2,

        /// Two i8, not normalised.
        Char2,

        /// Two u8, normalised.
        UChar2Norm,

        /// Two i8, normalised.
        Char2Norm,

        /// Three u8, not normalised.
        UChar3,

        /// Three i8, not normalised.
        Char3,

        /// Three u8, normalised.
        UChar3Norm,

        /// Three i8, normalised.
        Char3Norm,

        /// Four i8, not normalised.
        Char4,

        /// One u16, not normalised.
        UShort,

        /// One i16, not normalised.
        Short,

        /// One u16, normalised.
        UShortNorm,

        /// One i16, normalised.
        ShortNorm,

        /// Two u16, not normalised.
        UShort2,

        /// Two i16, not normalised.
        Short2,

        /// Three u16, not normalised.
        UShort3,

        /// Three i16, not normalised.
        Short3,

        /// Three u16, normalised.
        UShort3Norm,

        /// Three i16, normalised.
        Short3Norm,

        /// One f16.
        Half,

        /// Three f16.
        Half3,

        /// 11-11-10 float.
        FloatRg11B10,
    }
}

plain_enum! {
    /// How often a vertex buffer advances.
    StepFunction {        /// Never, every vertex reads the same data.
        Constant,

        /// Once per vertex.
        PerVertex,

        /// Once per instance (or per `step_rate` instances).
        PerInstance,
    }
}

/// The blending of one colour attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlendState {
    /// The factor for the source colour.
    pub src_rgb: BlendFactor,
    /// The factor for the destination colour.
    pub dst_rgb: BlendFactor,
    /// The operation on the colour.
    pub op_rgb: BlendOp,
    /// The factor for the source alpha.
    pub src_alpha: BlendFactor,
    /// The factor for the destination alpha.
    pub dst_alpha: BlendFactor,
    /// The operation on the alpha.
    pub op_alpha: BlendOp,
}

impl BlendState {
    /// Classic alpha blending: source alpha over the destination.
    pub const ALPHA: Self = Self {
        src_rgb: BlendFactor::SrcAlpha,
        dst_rgb: BlendFactor::OneMinusSrcAlpha,
        op_rgb: BlendOp::Add,
        src_alpha: BlendFactor::One,
        dst_alpha: BlendFactor::OneMinusSrcAlpha,
        op_alpha: BlendOp::Add,
    };
}

/// One colour attachment of a render pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColorTarget {
    /// The pixel format.
    pub format: Format,
    /// The blending, or none for plain replacement.
    pub blend: Option<BlendState>,
    /// The channels that are written, as bits R=1, G=2, B=4, A=8.
    pub write_mask: u8,
}

impl ColorTarget {
    /// A target without blending that writes all channels.
    pub const fn new(format: Format) -> Self {
        Self {
            format,
            blend: None,
            write_mask: 0xF,
        }
    }

    /// The same target with blending.
    pub const fn with_blend(mut self, blend: BlendState) -> Self {
        self.blend = Some(blend);
        self
    }
}

/// One vertex attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexAttribute {
    /// The data format.
    pub format: VertexFormat,
    /// The byte offset inside the vertex.
    pub offset: u32,
    /// The vertex buffer slot.
    pub buffer: u32,
}

/// One vertex buffer layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexLayout {
    /// The vertex buffer slot.
    pub buffer: u32,
    /// The distance between two vertices in bytes.
    pub stride: u32,
    /// How often the buffer advances.
    pub step: StepFunction,
    /// The step rate for `PerInstance`.
    pub step_rate: u32,
}

/// The vertex input of a pipeline. An empty descriptor means the shader fetches its own vertices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct VertexDescriptor {
    /// The attributes, by attribute index.
    pub attributes: Vec<(u32, VertexAttribute)>,
    /// The buffer layouts.
    pub layouts: Vec<VertexLayout>,
}
