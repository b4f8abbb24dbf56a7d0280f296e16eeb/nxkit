//! Fixed-function state: blending, depth and stencil, the vertex layout and the rasteriser settings.
//!
//! All types are plain data so the frame encoder can build them from the game's registers and use them as keys. The
//! pipeline state (blend, topology, vertex layout) is `nxkit-pipestate`'s, shared with the build. This module maps it
//! to Metal.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTL4BlendState, MTLBlendFactor, MTLBlendOperation, MTLColorWriteMask, MTLCompareFunction,
    MTLDepthClipMode, MTLDepthStencilDescriptor, MTLDepthStencilState, MTLDevice,
    MTLPrimitiveTopologyClass, MTLStencilDescriptor, MTLStencilOperation, MTLTriangleFillMode,
    MTLVertexDescriptor, MTLVertexFormat, MTLVertexStepFunction, MTLWinding,
};

use crate::convert::Mtl;
use crate::device::Context;
use crate::error::{MetalError, Result, create};

pub use nxkit_pipestate::{
    BlendFactor, BlendOp, BlendState, ColorTarget, StepFunction, TopologyClass, VertexAttribute,
    VertexDescriptor, VertexFormat, VertexLayout,
};

macro_rules! mtl_map {
    ($name:ident => $mtl:ty { $($v:ident => $mv:ident),+ $(,)? }) => {
        impl Mtl for $name {
            type Out = $mtl;

            fn to_mtl(self) -> $mtl {
                match self { $(Self::$v => <$mtl>::$mv),+ }
            }
        }
    };
}

macro_rules! mtl_enum {
    ($(#[$m:meta])* $name:ident => $mtl:ty { $($(#[$vm:meta])* $v:ident => $mv:ident),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $v),+ }
        impl $name {
            /// Every value, in declaration order (for a text form that is read back).
            pub const ALL: &'static [Self] = &[$(Self::$v),+];

            pub(crate) fn to_mtl(self) -> $mtl {
                match self { $(Self::$v => <$mtl>::$mv),+ }
            }
        }
    };
}

mtl_map! {
    BlendFactor => MTLBlendFactor {
        Zero => Zero,
        One => One,
        SrcColor => SourceColor,
        OneMinusSrcColor => OneMinusSourceColor,
        SrcAlpha => SourceAlpha,
        OneMinusSrcAlpha => OneMinusSourceAlpha,
        DstColor => DestinationColor,
        OneMinusDstColor => OneMinusDestinationColor,
        DstAlpha => DestinationAlpha,
        OneMinusDstAlpha => OneMinusDestinationAlpha,
        SrcAlphaSaturated => SourceAlphaSaturated,
        BlendColor => BlendColor,
        OneMinusBlendColor => OneMinusBlendColor,
        BlendAlpha => BlendAlpha,
        OneMinusBlendAlpha => OneMinusBlendAlpha,
        Src1Color => Source1Color,
        OneMinusSrc1Color => OneMinusSource1Color,
        Src1Alpha => Source1Alpha,
        OneMinusSrc1Alpha => OneMinusSource1Alpha,
    }
}

mtl_map! {
    BlendOp => MTLBlendOperation {
        Add => Add,
        Subtract => Subtract,
        ReverseSubtract => ReverseSubtract,
        Min => Min,
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

mtl_map! {
    TopologyClass => MTLPrimitiveTopologyClass {
        Unspecified => Unspecified,
        Point => Point,
        Line => Line,
        Triangle => Triangle,
    }
}

mtl_map! {
    VertexFormat => MTLVertexFormat {
        Float => Float,
        Float2 => Float2,
        Float3 => Float3,
        Float4 => Float4,
        UInt => UInt,
        UInt2 => UInt2,
        UInt3 => UInt3,
        UInt4 => UInt4,
        Int => Int,
        Int2 => Int2,
        Int3 => Int3,
        Int4 => Int4,
        Half2 => Half2,
        Half4 => Half4,
        UChar4Norm => UChar4Normalized,
        UChar4NormBgra => UChar4Normalized_BGRA,
        Char4Norm => Char4Normalized,
        UShort2Norm => UShort2Normalized,
        UShort4Norm => UShort4Normalized,
        Short2Norm => Short2Normalized,
        Short4Norm => Short4Normalized,
        UChar4 => UChar4,
        UShort4 => UShort4,
        Short4 => Short4,
        Int1010102Norm => Int1010102Normalized,
        UInt1010102Norm => UInt1010102Normalized,
        UChar => UChar,
        Char => Char,
        UCharNorm => UCharNormalized,
        CharNorm => CharNormalized,
        UChar2 => UChar2,
        Char2 => Char2,
        UChar2Norm => UChar2Normalized,
        Char2Norm => Char2Normalized,
        UChar3 => UChar3,
        Char3 => Char3,
        UChar3Norm => UChar3Normalized,
        Char3Norm => Char3Normalized,
        Char4 => Char4,
        UShort => UShort,
        Short => Short,
        UShortNorm => UShortNormalized,
        ShortNorm => ShortNormalized,
        UShort2 => UShort2,
        Short2 => Short2,
        UShort3 => UShort3,
        Short3 => Short3,
        UShort3Norm => UShort3Normalized,
        Short3Norm => Short3Normalized,
        Half => Half,
        Half3 => Half3,
        FloatRg11B10 => FloatRG11B10,
    }
}

mtl_map! {
    StepFunction => MTLVertexStepFunction {
        Constant => Constant,
        PerVertex => PerVertex,
        PerInstance => PerInstance,
    }
}

/// The Metal side of a colour target.
pub(crate) trait ColorTargetMtl {
    /// The write mask.
    fn mask(&self) -> MTLColorWriteMask;
    /// Whether blending is on.
    fn blend_state(&self) -> MTL4BlendState;
}

impl ColorTargetMtl for ColorTarget {
    fn mask(&self) -> MTLColorWriteMask {
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

    fn blend_state(&self) -> MTL4BlendState {
        if self.blend.is_some() {
            MTL4BlendState::Enabled
        } else {
            MTL4BlendState::Disabled
        }
    }
}

/// The Metal side of a vertex layout.
pub(crate) trait VertexDescriptorMtl {
    /// The Metal descriptor, or none for an empty layout.
    fn build(&self) -> Option<Retained<MTLVertexDescriptor>>;
}

impl VertexDescriptorMtl for VertexDescriptor {
    fn build(&self) -> Option<Retained<MTLVertexDescriptor>> {
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
                o.setStepRate(if l.step == StepFunction::Constant {
                    0
                } else {
                    l.step_rate.max(1) as usize
                });
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

// SAFETY: a depth and stencil state is an immutable, thread safe Metal object.
unsafe impl Send for DepthStencilState {}
// SAFETY: see `Send`.
unsafe impl Sync for DepthStencilState {}

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
