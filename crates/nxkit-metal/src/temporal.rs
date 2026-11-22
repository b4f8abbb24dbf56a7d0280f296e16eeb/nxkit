//! MetalFX temporal upscaling.
//!
//! [`TemporalUpscaler`] scales a jittered picture of the game up and accumulates it over frames. Unlike the spatial
//! scaler it needs, for every frame, the depth, the motion of each pixel to its place in the frame before and the
//! sub-pixel offset of the camera (the jitter). The caller supplies all of them, typed, so none can be left out. A game
//! that has no motion texture gets one from a pass of the caller (camera motion from depth and matrices).
//!
//! The scaler runs between passes, on a Metal 4 command buffer with no pass open. The caller orders it after the passes
//! that write its inputs and before the passes that read its output, as for [`crate::SpatialUpscaler`].

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::MTLTexture;
use objc2_metal_fx::{
    MTL4FXTemporalScaler, MTLFXTemporalScalerBase, MTLFXTemporalScalerDescriptor,
};

use crate::device::Context;
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result, create};
use crate::format::Format;
use crate::texture::Texture;

/// What a temporal scaler is made for. The sizes and formats cannot change after that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemporalDesc {
    /// The largest input size in pixels: colour, depth, motion and the reactive mask.
    pub input: (u32, u32),
    /// The output size in pixels.
    pub output: (u32, u32),
    /// The format of the colour texture.
    pub color: Format,
    /// The format of the depth texture.
    pub depth: Format,
    /// The format of the motion texture (two channels).
    pub motion: Format,
    /// The format of the output texture.
    pub output_format: Format,
    /// The format of the reactive mask, `None` for a scaler without one.
    pub reactive: Option<Format>,
    /// True when the colour holds linear light (HDR) instead of display values.
    pub hdr: bool,
}

/// The values of one frame. The textures are the caller's and must match the description.
pub struct TemporalFrame<'a> {
    /// The jittered picture.
    pub color: &'a Texture,
    /// Its depth.
    pub depth: &'a Texture,
    /// The motion of each pixel, in pixels of the input times `motion_scale`, pointing to where the pixel was in the
    /// frame before.
    pub motion: &'a Texture,
    /// The reactive mask, where 1 lets the current frame win. Only for a scaler made with one.
    pub reactive: Option<&'a Texture>,
    /// The part of the inputs that holds the picture, from the top-left corner. MetalFX refused a scaler with the input
    /// content properties on (nil on 2026-11-22, macOS 27.2), so the content is the input size of the description.
    pub content: (u32, u32),
    /// The jitter of the frame in input pixels, `x` to the right and `y` down.
    pub jitter: (f32, f32),
    /// The factors that turn a texel of the motion texture into input pixels.
    pub motion_scale: (f32, f32),
    /// True when the depth is reversed: 1 at the near plane.
    pub depth_reversed: bool,
    /// True on a cut: the history is thrown away.
    pub reset: bool,
}

struct Inner {
    raw: Retained<ProtocolObject<dyn MTL4FXTemporalScaler>>,
}

// SAFETY: the effect object is only used while its owner encodes, on one thread at a time, and the command buffer's
// retained copy of the `Arc` keeps it alive and may drop it on a Metal thread, which Metal allows for its objects.
unsafe impl Send for Inner {}
// SAFETY: see `Send`.
unsafe impl Sync for Inner {}

/// A MetalFX temporal scaler.
pub struct TemporalUpscaler {
    inner: Arc<Inner>,
    desc: TemporalDesc,
}

impl TemporalUpscaler {
    /// True when the device supports temporal scaling with a Metal 4 command buffer.
    pub fn supported(ctx: &Context) -> bool {
        // SAFETY: the device is a live Metal device.
        unsafe { MTLFXTemporalScalerDescriptor::supportsMetal4FX(ctx.device()) }
    }

    /// The smallest and the largest scale (output over input on one side) that the device takes.
    pub fn scale_range(ctx: &Context) -> (f32, f32) {
        // SAFETY: the device is a live Metal device.
        unsafe {
            (
                MTLFXTemporalScalerDescriptor::supportedInputContentMinScaleForDevice(ctx.device()),
                MTLFXTemporalScalerDescriptor::supportedInputContentMaxScaleForDevice(ctx.device()),
            )
        }
    }

    /// Makes a scaler.
    pub fn new(ctx: &Context, desc: TemporalDesc) -> Result<Self> {
        if desc.input.0 == 0 || desc.input.1 == 0 || desc.output.0 == 0 || desc.output.1 == 0 {
            return Err(MetalError::Invalid("a size of the temporal scaler is zero"));
        }
        if !desc.depth.is_depth() {
            return Err(MetalError::Invalid(
                "the depth input of the temporal scaler needs a depth format",
            ));
        }
        if !Self::supported(ctx) {
            return Err(create(
                "MetalFX temporal scaler",
                "the device does not support it with Metal 4",
            ));
        }
        // SAFETY: `new` makes a fresh descriptor.
        let d = unsafe { MTLFXTemporalScalerDescriptor::new() };
        // SAFETY: the descriptor is a fresh object that nothing else uses. The sizes are not zero.
        unsafe {
            d.setColorTextureFormat(desc.color.to_mtl());
            d.setDepthTextureFormat(desc.depth.to_mtl());
            d.setMotionTextureFormat(desc.motion.to_mtl());
            d.setOutputTextureFormat(desc.output_format.to_mtl());
            d.setInputWidth(desc.input.0 as usize);
            d.setInputHeight(desc.input.1 as usize);
            d.setOutputWidth(desc.output.0 as usize);
            d.setOutputHeight(desc.output.1 as usize);
            d.setAutoExposureEnabled(desc.hdr);
            if let Some(f) = desc.reactive {
                d.setReactiveMaskTextureEnabled(true);
                d.setReactiveMaskTextureFormat(f.to_mtl());
            }
        }
        // SAFETY: the device and the compiler are live and belong together.
        let raw = unsafe { d.newTemporalScalerWithDevice_compiler(ctx.device(), ctx.compiler()) }
            .ok_or_else(|| {
            create(
                "MetalFX temporal scaler",
                "newTemporalScalerWithDevice returned nil",
            )
        })?;
        Ok(Self {
            inner: Arc::new(Inner { raw }),
            desc,
        })
    }

    /// What the scaler was made for.
    pub fn desc(&self) -> TemporalDesc {
        self.desc
    }

    /// Encodes the frame. The scaler writes the whole output.
    ///
    /// No pass may be open on the command buffer. Each texture needs the usage that MetalFX asks for (the error names
    /// the input that lacks it).
    pub fn encode(
        &self,
        cmd: &mut CommandBuffer,
        f: &TemporalFrame<'_>,
        output: &Texture,
    ) -> Result<()> {
        self.check(f, output)?;
        let raw = &self.inner.raw;
        // SAFETY: the scaler is live. These are plain getters.
        let need = unsafe {
            [
                (
                    "the colour input lacks a usage that MetalFX needs",
                    raw.colorTextureUsage(),
                    Some(f.color),
                ),
                (
                    "the depth input lacks a usage that MetalFX needs",
                    raw.depthTextureUsage(),
                    Some(f.depth),
                ),
                (
                    "the motion input lacks a usage that MetalFX needs",
                    raw.motionTextureUsage(),
                    Some(f.motion),
                ),
                (
                    "the reactive mask lacks a usage that MetalFX needs",
                    raw.reactiveMaskTextureUsage(),
                    f.reactive,
                ),
                (
                    "the output lacks a usage that MetalFX needs",
                    raw.outputTextureUsage(),
                    Some(output),
                ),
            ]
        };
        for (why, usage, tex) in need {
            if tex.is_some_and(|t| !t.raw().usage().contains(usage)) {
                return Err(MetalError::Invalid(why));
            }
        }
        // SAFETY: the textures are live and match the description. The command buffer keeps them and the scaler alive
        // until the GPU is done, and the caller has no pass open on it.
        unsafe {
            raw.setColorTexture(Some(f.color.raw()));
            raw.setDepthTexture(Some(f.depth.raw()));
            raw.setMotionTexture(Some(f.motion.raw()));
            raw.setReactiveMaskTexture(f.reactive.map(Texture::raw));
            raw.setOutputTexture(Some(output.raw()));
            raw.setInputContentWidth(f.content.0 as usize);
            raw.setInputContentHeight(f.content.1 as usize);
            raw.setJitterOffsetX(f.jitter.0);
            raw.setJitterOffsetY(f.jitter.1);
            raw.setMotionVectorScaleX(f.motion_scale.0);
            raw.setMotionVectorScaleY(f.motion_scale.1);
            raw.setDepthReversed(f.depth_reversed);
            raw.setReset(f.reset);
            raw.encodeToCommandBuffer(cmd.raw());
        }
        cmd.retain((
            self.inner.clone(),
            f.color.clone(),
            f.depth.clone(),
            f.motion.clone(),
            f.reactive.cloned(),
            output.clone(),
        ));
        Ok(())
    }

    fn check(&self, f: &TemporalFrame<'_>, output: &Texture) -> Result<()> {
        let d = &self.desc;
        let fits = |t: &Texture, format: Format| {
            let x = t.desc();
            x.format == format && x.width >= d.input.0 && x.height >= d.input.1
        };
        if !fits(f.color, d.color) {
            return Err(MetalError::Invalid(
                "the colour input differs from the temporal scaler's description",
            ));
        }
        if !fits(f.depth, d.depth) {
            return Err(MetalError::Invalid(
                "the depth input differs from the temporal scaler's description",
            ));
        }
        if !fits(f.motion, d.motion) {
            return Err(MetalError::Invalid(
                "the motion input differs from the temporal scaler's description",
            ));
        }
        match (f.reactive, d.reactive) {
            (None, None) => {}
            (Some(t), Some(format)) if fits(t, format) => {}
            _ => {
                return Err(MetalError::Invalid(
                    "the reactive mask differs from the temporal scaler's description",
                ));
            }
        }
        let o = output.desc();
        if (o.width, o.height) != d.output || o.format != d.output_format {
            return Err(MetalError::Invalid(
                "the temporal scaler's output differs from its description",
            ));
        }
        if f.content.0 == 0
            || f.content.1 == 0
            || f.content.0 > d.input.0
            || f.content.1 > d.input.1
        {
            return Err(MetalError::Invalid(
                "the content size of the temporal scaler is outside the input",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
