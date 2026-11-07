//! Present: a `CAMetalLayer` drawable path.
//!
//! The window belongs to `nxkit-host`. This module takes a layer, hands out drawables and presents them with a
//! frame. A tick of the display link hands out the drawable, and `Frame::present_at` shows it at the target time of
//! the tick. [`crate::DisplayLink`] and [`crate::Pacer`] drive the ticks.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CGSize;
use objc2_metal::{MTLDrawable, MTLPixelFormat, MTLTexture};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use crate::device::Context;
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result};
use crate::format::Format;
use crate::frame::{Frame, PendingPresent};
use crate::texture::{Storage, Texture, TextureDesc};

/// The pixel format of the layer.
pub const PRESENT_FORMAT: Format = Format::Bgra8Unorm;

/// A drawable of the layer. Render into [`Drawable::texture`], then call [`Frame::present`].
pub struct Drawable {
    texture: Texture,
    raw: Retained<ProtocolObject<dyn CAMetalDrawable>>,
}

impl Drawable {
    pub(crate) fn wrap(ctx: &Context, raw: Retained<ProtocolObject<dyn CAMetalDrawable>>) -> Self {
        let tex = raw.texture();
        let mut desc =
            TextureDesc::render_target_2d(tex.width() as u32, tex.height() as u32, PRESENT_FORMAT);
        desc.shader_read = false;
        desc.storage = Storage::Private;
        Drawable {
            texture: Texture::wrap_external(ctx, tex, desc),
            raw,
        }
    }

    /// The texture to render into.
    pub fn texture(&self) -> &Texture {
        &self.texture
    }
}

/// The layer and its settings.
pub struct Presenter {
    layer: Retained<CAMetalLayer>,
    ctx: Context,
}

impl Presenter {
    /// Takes a `CAMetalLayer` of the window and sets it up for this device.
    ///
    /// # Safety
    ///
    /// `layer` must point to a live `CAMetalLayer`. The presenter retains it.
    pub unsafe fn from_raw_layer(ctx: &Context, layer: NonNull<c_void>) -> Result<Self> {
        // SAFETY: the caller guarantees that the pointer is a live `CAMetalLayer`.
        let layer = unsafe { Retained::retain(layer.cast::<CAMetalLayer>().as_ptr()) }
            .ok_or(MetalError::Invalid("null layer"))?;
        Ok(Self::setup(ctx, layer))
    }

    /// Makes a layer that is not in a window. It gives real drawables, which tests and capture tools use.
    pub fn offscreen(ctx: &Context, width: u32, height: u32) -> Self {
        let p = Self::setup(ctx, CAMetalLayer::new());
        p.resize(width, height);
        p
    }

    fn setup(ctx: &Context, layer: Retained<CAMetalLayer>) -> Self {
        layer.setDevice(Some(ctx.device()));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);
        layer.setMaximumDrawableCount(3);
        ctx.add_layer_residency(&layer.residencySet());
        Self {
            layer,
            ctx: ctx.clone(),
        }
    }

    /// Sets the size of the drawables in pixels.
    pub fn resize(&self, width: u32, height: u32) {
        self.layer.setDrawableSize(CGSize {
            width: f64::from(width),
            height: f64::from(height),
        });
    }

    /// Turns the display sync of the layer on or off.
    pub fn set_display_sync(&self, on: bool) {
        self.layer.setDisplaySyncEnabled(on);
    }

    /// The next drawable. It blocks while all drawables are in use.
    pub fn acquire(&self) -> Result<Drawable> {
        let raw = self
            .layer
            .nextDrawable()
            .ok_or(MetalError::Invalid("the layer gave no drawable"))?;
        Ok(Drawable::wrap(&self.ctx, raw))
    }

    pub(crate) fn layer(&self) -> &CAMetalLayer {
        &self.layer
    }

    pub(crate) fn context(&self) -> &Context {
        &self.ctx
    }
}

impl Frame {
    /// Presents the drawable when the frame is submitted. The queue waits for the drawable first.
    pub fn present(&mut self, drawable: Drawable) {
        let raw: Retained<ProtocolObject<dyn MTLDrawable>> =
            ProtocolObject::from_retained(drawable.raw.clone());
        self.set_present(PendingPresent {
            drawable: raw,
            keep: Box::new(drawable.texture),
            at: None,
        });
    }

    /// Presents the drawable at `at` when the frame is submitted, in seconds on the `CACurrentMediaTime` clock.
    ///
    /// A tick of the display link gives the time as `DisplayTick::target_presentation_timestamp`. Metal shows the
    /// frame on the first display refresh at or after that time. A time that has passed shows the frame on the next
    /// refresh.
    pub fn present_at(&mut self, drawable: Drawable, at: f64) {
        let raw: Retained<ProtocolObject<dyn MTLDrawable>> =
            ProtocolObject::from_retained(drawable.raw.clone());
        self.set_present(PendingPresent {
            drawable: raw,
            keep: Box::new(drawable.texture),
            at: Some(at),
        });
    }
}

impl CommandBuffer {
    /// Keeps the drawable's texture alive until the GPU is done.
    pub fn retain_drawable(&mut self, d: &Drawable) {
        self.retain(d.texture.clone());
    }
}
