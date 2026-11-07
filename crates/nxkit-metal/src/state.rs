//! Fixed-function state: blending, depth and stencil, the vertex layout and the rasteriser settings.
//!
//! All types are plain data so the frame encoder can build them from the game's registers and use them as keys.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTL4BlendState, MTLBlendFactor, MTLBlendOperation, MTLColorWriteMask, MTLCompareFunction,
    MTLDepthClipMode, MTLDepthStencilDescriptor, MTLDepthStencilState, MTLDevice,
    MTLPrimitiveTopologyClass, MTLStencilDescriptor, MTLStencilOperation, MTLTriangleFillMode,
    MTLVertexDescriptor, MTLVertexFormat, MTLVertexStepFunction, MTLWinding,
};

use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::format::Format;

macro_rules! mtl_enum {
    ($(#[$m:meta])* $name:ident => $mtl:ty { $($(#[$vm:meta])* $v:ident => $mv:ident),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $v),+ }
        impl $name {
            pub(crate) fn to_mtl(self) -> $mtl {
                match self { $(Self::$v => <$mtl>::$mv),+ }
            }
        }
    };
}

mtl_enum! {
    /// A blend factor.
    BlendFactor => MTLBlendFactor {
        /// 0.
        Zero => Zero,
        /// 1.
        One => One,
        /// The source colour.
        SrcColor => SourceColor,
        /// 1 minus the source colour.
        OneMinusSrcColor => OneMinusSourceColor,
        /// The source alpha.
        SrcAlpha => SourceAlpha,
        /// 1 minus the source alpha.
        OneMinusSrcAlpha => OneMinusSourceAlpha,
        /// The destination colour.
        DstColor => DestinationColor,
        /// 1 minus the destination colour.
        OneMinusDstColor => OneMinusDestinationColor,
        /// The destination alpha.
        DstAlpha => DestinationAlpha,
        /// 1 minus the destination alpha.
        OneMinusDstAlpha => OneMinusDestinationAlpha,
        /// The source alpha, saturated.
        SrcAlphaSaturated => SourceAlphaSaturated,
        /// The constant blend colour.
        BlendColor => BlendColor,
        /// 1 minus the constant blend colour.
        OneMinusBlendColor => OneMinusBlendColor,
        /// The constant blend alpha.
        BlendAlpha => BlendAlpha,
        /// 1 minus the constant blend alpha.
        OneMinusBlendAlpha => OneMinusBlendAlpha,
        /// The second fragment output colour (dual-source blending).
        Src1Color => Source1Color,
        /// 1 minus the second fragment output colour.
        OneMinusSrc1Color => OneMinusSource1Color,
        /// The second fragment output alpha.
        Src1Alpha => Source1Alpha,
        /// 1 minus the second fragment output alpha.
        OneMinusSrc1Alpha => OneMinusSource1Alpha,
    }
}

mtl_enum! {
    /// A blend operation.
    BlendOp => MTLBlendOperation {
        /// source + destination.
        Add => Add,
        /// source - destination.
        Subtract => Subtract,
        /// destination - source.
        ReverseSubtract => ReverseSubtract,
        /// The smaller value.
        Min => Min,
        /// The larger value.
        Max => Max,
    }
}

mtl_enum! {
    /// A depth or stencil comparison.
    CompareFn => MTLCompareFunction {
        /// Never passes.
        Never => Never,
        /// Passes when less.
        Less => Less,
        /// Passes when equal.
        Equal => Equal,
        /// Passes when less or equal.
        LessEqual => LessEqual,
        /// Passes when greater.
        Greater => Greater,
        /// Passes when not equal.
        NotEqual => NotEqual,
        /// Passes when greater or equal.
        GreaterEqual => GreaterEqual,
        /// Always passes.
        Always => Always,
    }
}

mtl_enum! {
    /// What happens to a stencil value.
    StencilOp => MTLStencilOperation {
        /// Keep it.
        Keep => Keep,
        /// Set it to 0.
        Zero => Zero,
        /// Set it to the reference value.
        Replace => Replace,
        /// Add 1 and clamp.
        IncrementClamp => IncrementClamp,
        /// Subtract 1 and clamp.
        DecrementClamp => DecrementClamp,
        /// Invert the bits.
        Invert => Invert,
        /// Add 1 and wrap.
        IncrementWrap => IncrementWrap,
        /// Subtract 1 and wrap.
        DecrementWrap => DecrementWrap,
    }
}

mtl_enum! {
    /// Which winding order is a front face.
    Winding => MTLWinding {
        /// Clockwise.
        Clockwise => Clockwise,
        /// Counter-clockwise.
        CounterClockwise => CounterClockwise,
    }
}

mtl_enum! {
    /// How triangles are filled.
    FillMode => MTLTriangleFillMode {
        /// Filled.
        Fill => Fill,
        /// Wireframe.
        Lines => Lines,
    }
}

mtl_enum! {
    /// What happens to a fragment outside the depth range.
    DepthClip => MTLDepthClipMode {
        /// It is clipped.
        Clip => Clip,
        /// Its depth is clamped.
        Clamp => Clamp,
    }
}

mtl_enum! {
    /// The primitive class a pipeline draws. Metal needs it when the pipeline is built.
    TopologyClass => MTLPrimitiveTopologyClass {
        /// Points.
        Point => Point,
        /// Lines.
        Line => Line,
        /// Triangles.
        Triangle => Triangle,
    }
}

mtl_enum! {
    /// The format of one vertex attribute.
    VertexFormat => MTLVertexFormat {
        /// One f32.
        Float => Float,
        /// Two f32.
        Float2 => Float2,
        /// Three f32.
        Float3 => Float3,
        /// Four f32.
        Float4 => Float4,
        /// One u32.
        UInt => UInt,
        /// Two u32.
        UInt2 => UInt2,
        /// Three u32.
        UInt3 => UInt3,
        /// Four u32.
        UInt4 => UInt4,
        /// One i32.
        Int => Int,
        /// Two i32.
        Int2 => Int2,
        /// Three i32.
        Int3 => Int3,
        /// Four i32.
        Int4 => Int4,
        /// Two f16.
        Half2 => Half2,
        /// Four f16.
        Half4 => Half4,
        /// Four u8, normalised.
        UChar4Norm => UChar4Normalized,
        /// Four u8, normalised, in BGRA order.
        UChar4NormBgra => UChar4Normalized_BGRA,
        /// Four i8, normalised.
        Char4Norm => Char4Normalized,
        /// Two u16, normalised.
        UShort2Norm => UShort2Normalized,
        /// Four u16, normalised.
        UShort4Norm => UShort4Normalized,
        /// Two i16, normalised.
        Short2Norm => Short2Normalized,
        /// Four i16, normalised.
        Short4Norm => Short4Normalized,
        /// Four u8, not normalised.
        UChar4 => UChar4,
        /// Four u16, not normalised.
        UShort4 => UShort4,
        /// Four i16, not normalised.
        Short4 => Short4,
        /// 10-10-10-2 signed, normalised.
        Int1010102Norm => Int1010102Normalized,
        /// 10-10-10-2 unsigned, normalised.
        UInt1010102Norm => UInt1010102Normalized,
    }
}

mtl_enum! {
    /// How often a vertex buffer advances.
    StepFunction => MTLVertexStepFunction {
        /// Never, every vertex reads the same data.
        Constant => Constant,
        /// Once per vertex.
        PerVertex => PerVertex,
        /// Once per instance (or per `step_rate` instances).
        PerInstance => PerInstance,
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

    pub(crate) fn mask(&self) -> MTLColorWriteMask {
        let mut m = MTLColorWriteMask::None;
        for (bit, v) in [
            (1, MTLColorWriteMask::Red),
            (2, MTLColorWriteMask::Green),
            (4, MTLColorWriteMask::Blue),
            (8, MTLColorWriteMask::Alpha),
        ] {
            if self.write_mask & bit != 0 {
                m |= v;
            }
        }
        m
    }

    pub(crate) fn blend_state(&self) -> MTL4BlendState {
        if self.blend.is_some() {
            MTL4BlendState::Enabled
        } else {
            MTL4BlendState::Disabled
        }
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

impl VertexDescriptor {
    pub(crate) fn build(&self) -> Option<Retained<MTLVertexDescriptor>> {
        if self.attributes.is_empty() && self.layouts.is_empty() {
            return None;
        }
        let d = MTLVertexDescriptor::new();
        let attrs = d.attributes();
        for (i, a) in &self.attributes {
            // SAFETY: the slots and offsets are plain numbers. Metal checks the limits when it compiles the pipeline.
            unsafe {
                let o = attrs.objectAtIndexedSubscript(*i as usize);
                o.setFormat(a.format.to_mtl());
                o.setOffset(a.offset as usize);
                o.setBufferIndex(a.buffer as usize);
            }
        }
        let layouts = d.layouts();
        for l in &self.layouts {
            // SAFETY: the slot, stride and rate are plain numbers. Metal checks the limits when it compiles the pipeline.
            unsafe {
                let o = layouts.objectAtIndexedSubscript(l.buffer as usize);
                o.setStride(l.stride as usize);
                o.setStepFunction(l.step.to_mtl());
                o.setStepRate(l.step_rate.max(1) as usize);
            }
        }
        Some(d)
    }
}

/// The stencil test of one face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StencilFace {
    /// The comparison.
    pub compare: CompareFn,
    /// What happens when the stencil test fails.
    pub fail: StencilOp,
    /// What happens when the stencil passes and the depth test fails.
    pub depth_fail: StencilOp,
    /// What happens when both pass.
    pub pass: StencilOp,
    /// The bits that are read.
    pub read_mask: u32,
    /// The bits that are written.
    pub write_mask: u32,
}

impl StencilFace {
    fn build(&self) -> Retained<MTLStencilDescriptor> {
        let s = MTLStencilDescriptor::new();
        s.setStencilCompareFunction(self.compare.to_mtl());
        s.setStencilFailureOperation(self.fail.to_mtl());
        s.setDepthFailureOperation(self.depth_fail.to_mtl());
        s.setDepthStencilPassOperation(self.pass.to_mtl());
        s.setReadMask(self.read_mask);
        s.setWriteMask(self.write_mask);
        s
    }
}

/// The depth and stencil test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DepthStencilDesc {
    /// The depth comparison. `Always` with `depth_write` off turns the test off.
    pub depth_compare: CompareFn,
    /// Whether passing fragments write their depth.
    pub depth_write: bool,
    /// The stencil test of front faces, or none.
    pub front: Option<StencilFace>,
    /// The stencil test of back faces, or none.
    pub back: Option<StencilFace>,
}

impl DepthStencilDesc {
    /// No depth test and no stencil.
    pub const OFF: Self = Self {
        depth_compare: CompareFn::Always,
        depth_write: false,
        front: None,
        back: None,
    };
}

/// A depth and stencil state object. Make it once per distinct [`DepthStencilDesc`] and reuse it.
#[derive(Clone)]
pub struct DepthStencilState {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLDepthStencilState>>,
}

impl DepthStencilState {
    /// Makes the state object.
    pub fn new(ctx: &Context, desc: &DepthStencilDesc) -> Result<Self> {
        let d = MTLDepthStencilDescriptor::new();
        d.setDepthCompareFunction(desc.depth_compare.to_mtl());
        d.setDepthWriteEnabled(desc.depth_write);
        if let Some(f) = &desc.front {
            d.setFrontFaceStencil(Some(&f.build()));
        }
        if let Some(b) = &desc.back {
            d.setBackFaceStencil(Some(&b.build()));
        }
        let raw = ctx
            .device()
            .newDepthStencilStateWithDescriptor(&d)
            .ok_or_else(|| {
                create(
                    "depth stencil state",
                    "newDepthStencilStateWithDescriptor returned nil",
                )
            })?;
        Ok(Self { raw })
    }
}

/// The depth bias of the rasteriser.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthBias {
    /// The constant bias.
    pub constant: f32,
    /// The bias that scales with the slope.
    pub slope_scale: f32,
    /// The largest bias, or 0 for no limit.
    pub clamp: f32,
}

pub(crate) fn check_sample_count(n: u32) -> Result<()> {
    if matches!(n, 1 | 2 | 4 | 8) {
        Ok(())
    } else {
        Err(MetalError::Invalid("the sample count must be 1, 2, 4 or 8"))
    }
}
