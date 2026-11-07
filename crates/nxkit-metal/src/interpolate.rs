//! MetalFX frame interpolation (R29).
//!
//! [`FrameInterpolator`] makes the frame between two game frames, so a game at 60 Hz shows 120 frames per second on a
//! 120 Hz display. The frames come from the game. The in-between frame is made by MetalFX from the two frames and
//! from per-pixel motion and depth. Nothing is blended by the host.
//!
//! What MetalFX needs besides the two colour frames is typed, so a caller cannot leave it out: a depth texture, a
//! motion texture, the camera values and the time between the frames. A game gives none of these in general. The host
//! has to produce them for a title without a frame step. How it produces them for a game that has no motion vectors
//! is not settled. This module does not invent them, and it does not feed a zero texture in silence.
//!
//! The descriptor of MetalFX also takes an optional temporal scaler. This module leaves it unset. Whether the
//! interpolator is made without it is not verified on a device. [`FrameInterpolator::new`] returns an error that names
//! the call when Metal returns nil.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal_fx::{
    MTL4FXFrameInterpolator, MTLFXFrameInterpolatorBase, MTLFXFrameInterpolatorDescriptor,
};

use crate::device::Context;
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result, create};
use crate::format::Format;
use crate::texture::{Storage, Texture};

/// What an interpolator is made for. The sizes and formats cannot change after that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterpolateDesc {
    /// The size of the colour, depth and motion textures in pixels.
    pub input: (u32, u32),
    /// The size of the output in pixels.
    pub output: (u32, u32),
    /// The format of the two colour textures.
    pub color: Format,
    /// The format of the depth texture.
    pub depth: Format,
    /// The format of the motion texture, `Format::Rg16Float` for most renderers.
    pub motion: Format,
    /// The format of the output texture.
    pub output_format: Format,
}

/// The camera of the frames, as the perspective matrix of the game was made.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// The near plane distance.
    pub near: f32,
    /// The far plane distance.
    pub far: f32,
    /// The vertical field of view in degrees.
    pub fov_degrees: f32,
    /// Width divided by height.
    pub aspect: f32,
    /// True when depth 1 is the near plane.
    pub depth_reversed: bool,
}

/// The inputs of one interpolation.
pub struct InterpolateInputs<'a> {
    /// The newer game frame.
    pub color: &'a Texture,
    /// The older game frame.
    pub prev_color: &'a Texture,
    /// The depth of the newer frame.
    pub depth: &'a Texture,
    /// The motion of the newer frame, in pixels times `motion_scale`.
    pub motion: &'a Texture,
    /// Where the in-between frame goes.
    pub output: &'a Texture,
    /// The top-left part of the input textures to process, in pixels.
    pub content: (u32, u32),
    /// The seconds between the older and the newer frame.
    pub delta_time: f32,
    /// The factors that turn a motion value into pixels, horizontal and vertical.
    pub motion_scale: (f32, f32),
    /// The camera.
    pub camera: Camera,
    /// The sub-pixel jitter of the newer frame in pixels. It is zero without temporal anti-aliasing.
    pub jitter: (f32, f32),
    /// True after a scene cut, so MetalFX drops its history.
    pub reset_history: bool,
}

struct Inner {
    raw: Retained<ProtocolObject<dyn MTL4FXFrameInterpolator>>,
}

/// A MetalFX frame interpolator.
pub struct FrameInterpolator {
    inner: Arc<Inner>,
    desc: InterpolateDesc,
}

fn check_input(t: &Texture, what: &'static str, want: Format, size: (u32, u32)) -> Result<()> {
    let d = t.desc();
    if d.format != want || (d.width, d.height) != size {
        return Err(MetalError::Invalid(what));
    }
    if !d.shader_read {
        return Err(MetalError::Invalid(
            "an interpolator input needs shader read access",
        ));
    }
    Ok(())
}

impl FrameInterpolator {
    /// True when the device supports frame interpolation with a Metal 4 command buffer.
    pub fn supported(ctx: &Context) -> bool {
        // SAFETY: the device is a live Metal device.
        unsafe { MTLFXFrameInterpolatorDescriptor::supportsMetal4FX(ctx.device()) }
    }

    /// Makes an interpolator.
    pub fn new(ctx: &Context, desc: InterpolateDesc) -> Result<Self> {
        let sizes = [desc.input, desc.output];
        if sizes.iter().any(|(w, h)| *w == 0 || *h == 0) {
            return Err(MetalError::Invalid("a size of the interpolator is zero"));
        }
        if !Self::supported(ctx) {
            return Err(create(
                "MetalFX frame interpolator",
                "the device does not support it with Metal 4",
            ));
        }
        // SAFETY: `new` makes a fresh descriptor.
        let d = unsafe { MTLFXFrameInterpolatorDescriptor::new() };
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
            d.setRequiresPrevColorTexture(true);
        }
        // SAFETY: the device and the compiler are live and belong together.
        let raw =
            unsafe { d.newFrameInterpolatorWithDevice_compiler(ctx.device(), ctx.compiler()) }
                .ok_or_else(|| {
                    create(
                        "MetalFX frame interpolator",
                        "newFrameInterpolatorWithDevice returned nil",
                    )
                })?;
        Ok(Self {
            inner: Arc::new(Inner { raw }),
            desc,
        })
    }

    /// What the interpolator was made for.
    pub fn desc(&self) -> InterpolateDesc {
        self.desc
    }

    /// Encodes the in-between frame into `inputs.output`.
    ///
    /// No pass may be open on the command buffer. The caller adds the barriers that make the inputs visible and the
    /// output visible to the next pass. The inputs need shader read access, and the output needs shader write access
    /// and private storage.
    pub fn encode(&self, cmd: &mut CommandBuffer, inputs: &InterpolateInputs<'_>) -> Result<()> {
        let d = &self.desc;
        check_input(
            inputs.color,
            "the colour texture differs from the description",
            d.color,
            d.input,
        )?;
        check_input(
            inputs.prev_color,
            "the previous colour texture differs from the description",
            d.color,
            d.input,
        )?;
        check_input(
            inputs.depth,
            "the depth texture differs from the description",
            d.depth,
            d.input,
        )?;
        check_input(
            inputs.motion,
            "the motion texture differs from the description",
            d.motion,
            d.input,
        )?;
        let o = inputs.output.desc();
        if o.format != d.output_format || (o.width, o.height) != d.output {
            return Err(MetalError::Invalid(
                "the interpolator output differs from its description",
            ));
        }
        if !o.shader_write || o.storage != Storage::Private {
            return Err(MetalError::Invalid(
                "the interpolator output needs shader write access and private storage",
            ));
        }
        let (cw, ch) = inputs.content;
        if cw == 0 || ch == 0 || cw > d.input.0 || ch > d.input.1 {
            return Err(MetalError::Invalid(
                "the content size of the interpolator is outside the input",
            ));
        }
        if !(inputs.delta_time.is_finite() && inputs.delta_time > 0.0) {
            return Err(MetalError::Invalid(
                "the time between the frames must be positive",
            ));
        }
        let raw = &self.inner.raw;
        let c = &inputs.camera;
        // SAFETY: all textures are live and match the description. The command buffer keeps them and the interpolator
        // alive until the GPU is done, and the caller has no pass open on it.
        unsafe {
            raw.setColorTexture(Some(inputs.color.raw()));
            raw.setPrevColorTexture(Some(inputs.prev_color.raw()));
            raw.setDepthTexture(Some(inputs.depth.raw()));
            raw.setMotionTexture(Some(inputs.motion.raw()));
            raw.setOutputTexture(Some(inputs.output.raw()));
            raw.setContentWidth(cw as usize);
            raw.setContentHeight(ch as usize);
            raw.setDeltaTime(inputs.delta_time);
            raw.setMotionVectorScaleX(inputs.motion_scale.0);
            raw.setMotionVectorScaleY(inputs.motion_scale.1);
            raw.setNearPlane(c.near);
            raw.setFarPlane(c.far);
            raw.setFieldOfView(c.fov_degrees);
            raw.setAspectRatio(c.aspect);
            raw.setDepthReversed(c.depth_reversed);
            raw.setJitterOffsetX(inputs.jitter.0);
            raw.setJitterOffsetY(inputs.jitter.1);
            raw.setShouldResetHistory(inputs.reset_history);
            raw.encodeToCommandBuffer(cmd.raw());
        }
        cmd.retain((
            self.inner.clone(),
            inputs.color.clone(),
            inputs.prev_color.clone(),
            inputs.depth.clone(),
            inputs.motion.clone(),
            inputs.output.clone(),
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc() -> InterpolateDesc {
        InterpolateDesc {
            input: (64, 64),
            output: (64, 64),
            color: Format::Rgba8Unorm,
            depth: Format::Depth32Float,
            motion: Format::Rg16Float,
            output_format: Format::Rgba8Unorm,
        }
    }

    #[test]
    fn a_zero_size_is_refused_before_metal_is_asked() {
        let Ok(ctx) = Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let mut d = desc();
        d.output = (64, 0);
        assert!(FrameInterpolator::new(&ctx, d).is_err());
    }

    #[test]
    fn the_support_query_answers() {
        let Ok(ctx) = Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        // Either answer is valid. The query must not crash on a device with Metal 4.
        let _ = FrameInterpolator::supported(&ctx);
    }
}
