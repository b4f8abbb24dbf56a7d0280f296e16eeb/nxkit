//! Sampler states that shaders reach through a resource ID in an argument buffer.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLSamplerAddressMode, MTLSamplerBorderColor, MTLSamplerDescriptor,
    MTLSamplerMinMagFilter, MTLSamplerMipFilter, MTLSamplerReductionMode, MTLSamplerState,
};

use crate::device::Context;
use crate::error::{Result, create};
use crate::state::CompareFn;

/// A texture filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Filter {
    /// The nearest texel.
    Nearest,
    /// A linear blend.
    Linear,
}

/// How a sampler picks between mip levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MipFilter {
    /// Level 0 only.
    None,
    /// The nearest level.
    Nearest,
    /// A blend of two levels.
    Linear,
}

/// What a sampler does outside of 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wrap {
    /// The edge texel repeats.
    ClampToEdge,
    /// The texture mirrors once, then the edge repeats.
    MirrorClampToEdge,
    /// The texture repeats.
    Repeat,
    /// The texture mirrors and repeats.
    MirrorRepeat,
    /// Outside is the border colour.
    ClampToBorder,
}

/// The border colour of a sampler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Border {
    /// 0, 0, 0, 0.
    TransparentBlack,
    /// 0, 0, 0, 1.
    OpaqueBlack,
    /// 1, 1, 1, 1.
    OpaqueWhite,
}

/// How a filter combines texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reduction {
    /// The weighted average.
    Average,
    /// The smallest texel.
    Minimum,
    /// The largest texel.
    Maximum,
}

/// What a sampler does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerDesc {
    /// The filter for minification.
    pub min: Filter,
    /// The filter for magnification.
    pub mag: Filter,
    /// The filter between mip levels.
    pub mip: MipFilter,
    /// The wrap modes for S, T and R.
    pub wrap: [Wrap; 3],
    /// The border colour.
    pub border: Border,
    /// The anisotropy, 1 to 16.
    pub max_anisotropy: u32,
    /// The comparison of a depth sampler, or none.
    pub compare: Option<CompareFn>,
    /// The reduction of the filter.
    pub reduction: Reduction,
    /// The lowest mip level.
    pub lod_min: f32,
    /// The highest mip level.
    pub lod_max: f32,
    /// The bias added to the level.
    pub lod_bias: f32,
}

impl Default for SamplerDesc {
    fn default() -> Self {
        Self {
            min: Filter::Nearest,
            mag: Filter::Nearest,
            mip: MipFilter::None,
            wrap: [Wrap::ClampToEdge; 3],
            border: Border::TransparentBlack,
            max_anisotropy: 1,
            compare: None,
            reduction: Reduction::Average,
            lod_min: 0.0,
            lod_max: 1000.0,
            lod_bias: 0.0,
        }
    }
}

fn filter(f: Filter) -> MTLSamplerMinMagFilter {
    match f {
        Filter::Nearest => MTLSamplerMinMagFilter::Nearest,
        Filter::Linear => MTLSamplerMinMagFilter::Linear,
    }
}

fn wrap(w: Wrap) -> MTLSamplerAddressMode {
    match w {
        Wrap::ClampToEdge => MTLSamplerAddressMode::ClampToEdge,
        Wrap::MirrorClampToEdge => MTLSamplerAddressMode::MirrorClampToEdge,
        Wrap::Repeat => MTLSamplerAddressMode::Repeat,
        Wrap::MirrorRepeat => MTLSamplerAddressMode::MirrorRepeat,
        Wrap::ClampToBorder => MTLSamplerAddressMode::ClampToBorderColor,
    }
}

/// A sampler state. Clones share it.
#[derive(Clone)]
pub struct Sampler {
    raw: Retained<ProtocolObject<dyn MTLSamplerState>>,
}

// SAFETY: a sampler state is an immutable, thread safe Metal object.
unsafe impl Send for Sampler {}
// SAFETY: see `Send`.
unsafe impl Sync for Sampler {}

impl Sampler {
    /// Makes a sampler that can be named in an argument buffer.
    pub fn new(ctx: &Context, desc: &SamplerDesc) -> Result<Self> {
        let d = MTLSamplerDescriptor::new();
        d.setMinFilter(filter(desc.min));
        d.setMagFilter(filter(desc.mag));
        d.setMipFilter(match desc.mip {
            MipFilter::None => MTLSamplerMipFilter::NotMipmapped,
            MipFilter::Nearest => MTLSamplerMipFilter::Nearest,
            MipFilter::Linear => MTLSamplerMipFilter::Linear,
        });
        d.setSAddressMode(wrap(desc.wrap[0]));
        d.setTAddressMode(wrap(desc.wrap[1]));
        d.setRAddressMode(wrap(desc.wrap[2]));
        d.setBorderColor(match desc.border {
            Border::TransparentBlack => MTLSamplerBorderColor::TransparentBlack,
            Border::OpaqueBlack => MTLSamplerBorderColor::OpaqueBlack,
            Border::OpaqueWhite => MTLSamplerBorderColor::OpaqueWhite,
        });
        d.setMaxAnisotropy(desc.max_anisotropy.clamp(1, 16) as usize);
        d.setReductionMode(match desc.reduction {
            Reduction::Average => MTLSamplerReductionMode::WeightedAverage,
            Reduction::Minimum => MTLSamplerReductionMode::Minimum,
            Reduction::Maximum => MTLSamplerReductionMode::Maximum,
        });
        d.setLodMinClamp(desc.lod_min);
        d.setLodMaxClamp(desc.lod_max);
        d.setLodBias(desc.lod_bias);
        if let Some(c) = desc.compare {
            d.setCompareFunction(c.to_mtl());
        }
        d.setSupportArgumentBuffers(true);
        let raw = ctx
            .device()
            .newSamplerStateWithDescriptor(&d)
            .ok_or_else(|| create("sampler", "newSamplerStateWithDescriptor returned nil"))?;
        Ok(Self { raw })
    }

    /// The 64 bit resource ID that a shader reads from an argument buffer.
    pub fn gpu_resource_id(&self) -> u64 {
        self.raw.gpuResourceID().to_raw()
    }
}
