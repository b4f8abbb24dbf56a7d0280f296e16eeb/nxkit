//! MetalFX spatial upscaling, the fallback for heavy games (R29).
//!
//! The host renders the game below the drawable size and [`SpatialUpscaler`] scales the result up. It works from one
//! frame and needs no motion vectors, so any game can use it. A title asks for it with `Upscaler::MetalFx` in its
//! `upscaler()` hook. The scale comes from [`crate::choose_render_scale`].
//!
//! The scaler writes into a texture with shader write access. A drawable is a frame buffer only, so the host scales into
//! an intermediate texture and draws that into the drawable.
//!
//! The scaler is encoded between passes. The caller adds the barriers that make the input visible to it and the output
//! visible to the next pass (see `Dependency`). The need for these barriers with Metal 4 MetalFX is not verified on a
//! device.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal_fx::{
    MTL4FXSpatialScaler, MTLFXSpatialScalerBase, MTLFXSpatialScalerColorProcessingMode,
    MTLFXSpatialScalerDescriptor,
};

use crate::device::Context;
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result, create};
use crate::format::Format;
use crate::texture::Texture;

/// What a scaler is made for. The sizes and formats cannot change after that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpscaleDesc {
    /// The largest input size in pixels.
    pub input: (u32, u32),
    /// The output size in pixels.
    pub output: (u32, u32),
    /// The format of the input texture.
    pub input_format: Format,
    /// The format of the output texture.
    pub output_format: Format,
    /// True when the input holds linear light instead of display values.
    pub linear: bool,
}

struct Inner {
    raw: Retained<ProtocolObject<dyn MTL4FXSpatialScaler>>,
}

// SAFETY: the effect object is only used while its owner encodes, on one thread at a time, and the command buffer's
// retained copy of the `Arc` keeps it alive and may drop it on a Metal thread, which Metal allows for its objects.
unsafe impl Send for Inner {}
// SAFETY: see `Send`.
unsafe impl Sync for Inner {}

/// A MetalFX spatial scaler.
pub struct SpatialUpscaler {
    inner: Arc<Inner>,
    desc: UpscaleDesc,
}

impl SpatialUpscaler {
    /// True when the device supports spatial scaling with a Metal 4 command buffer.
    pub fn supported(ctx: &Context) -> bool {
        // SAFETY: the device is a live Metal device.
        unsafe { MTLFXSpatialScalerDescriptor::supportsMetal4FX(ctx.device()) }
    }

    /// Makes a scaler.
    pub fn new(ctx: &Context, desc: UpscaleDesc) -> Result<Self> {
        if desc.input.0 == 0 || desc.input.1 == 0 || desc.output.0 == 0 || desc.output.1 == 0 {
            return Err(MetalError::Invalid("a size of the upscaler is zero"));
        }
        if !Self::supported(ctx) {
            return Err(create(
                "MetalFX spatial scaler",
                "the device does not support it with Metal 4",
            ));
        }
        // SAFETY: `new` makes a fresh descriptor.
        let d = unsafe { MTLFXSpatialScalerDescriptor::new() };
        let mode = if desc.linear {
            MTLFXSpatialScalerColorProcessingMode::Linear
        } else {
            MTLFXSpatialScalerColorProcessingMode::Perceptual
        };
        // SAFETY: the descriptor is a fresh object that nothing else uses. The sizes are not zero.
        unsafe {
            d.setColorTextureFormat(desc.input_format.to_mtl());
            d.setOutputTextureFormat(desc.output_format.to_mtl());
            d.setInputWidth(desc.input.0 as usize);
            d.setInputHeight(desc.input.1 as usize);
            d.setOutputWidth(desc.output.0 as usize);
            d.setOutputHeight(desc.output.1 as usize);
            d.setColorProcessingMode(mode);
        }
        // SAFETY: the device and the compiler are live and belong together.
        let raw = unsafe { d.newSpatialScalerWithDevice_compiler(ctx.device(), ctx.compiler()) }
            .ok_or_else(|| {
                create(
                    "MetalFX spatial scaler",
                    "newSpatialScalerWithDevice returned nil",
                )
            })?;
        Ok(Self {
            inner: Arc::new(Inner { raw }),
            desc,
        })
    }

    /// What the scaler was made for.
    pub fn desc(&self) -> UpscaleDesc {
        self.desc
    }

    /// Encodes the scaling of the top-left `content` pixels of `input` into `output`.
    ///
    /// No pass may be open on the command buffer. The input needs shader read access. The output needs shader write
    /// access and the size and format of the description.
    pub fn encode(
        &self,
        cmd: &mut CommandBuffer,
        input: &Texture,
        content: (u32, u32),
        output: &Texture,
    ) -> Result<()> {
        let (i, o) = (input.desc(), output.desc());
        if (i.width, i.height) != self.desc.input || i.format != self.desc.input_format {
            return Err(MetalError::Invalid(
                "the upscaler input differs from its description",
            ));
        }
        if (o.width, o.height) != self.desc.output || o.format != self.desc.output_format {
            return Err(MetalError::Invalid(
                "the upscaler output differs from its description",
            ));
        }
        if !i.shader_read {
            return Err(MetalError::Invalid(
                "the upscaler input needs shader read access",
            ));
        }
        if !o.shader_write {
            return Err(MetalError::Invalid(
                "the upscaler output needs shader write access",
            ));
        }
        if content.0 == 0
            || content.1 == 0
            || content.0 > self.desc.input.0
            || content.1 > self.desc.input.1
        {
            return Err(MetalError::Invalid(
                "the content size of the upscaler is outside the input",
            ));
        }
        let raw = &self.inner.raw;
        // SAFETY: both textures are live and match the description. The command buffer keeps them and the scaler alive
        // until the GPU is done, and the caller has no pass open on it.
        unsafe {
            raw.setColorTexture(Some(input.raw()));
            raw.setOutputTexture(Some(output.raw()));
            raw.setInputContentWidth(content.0 as usize);
            raw.setInputContentHeight(content.1 as usize);
            raw.encodeToCommandBuffer(cmd.raw());
        }
        cmd.retain((self.inner.clone(), input.clone(), output.clone()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc() -> UpscaleDesc {
        UpscaleDesc {
            input: (64, 64),
            output: (128, 128),
            input_format: Format::Rgba8Unorm,
            output_format: Format::Rgba8Unorm,
            linear: false,
        }
    }

    #[test]
    fn a_zero_size_is_refused_before_metal_is_asked() {
        let Ok(ctx) = Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let mut d = desc();
        d.input = (0, 64);
        assert!(SpatialUpscaler::new(&ctx, d).is_err());
    }

    #[test]
    fn the_scaler_is_made_when_the_device_has_it() {
        let Ok(ctx) = Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        if !SpatialUpscaler::supported(&ctx) {
            eprintln!("skipped: no MetalFX spatial scaler");
            return;
        }
        assert!(SpatialUpscaler::new(&ctx, desc()).is_ok());
    }
}
