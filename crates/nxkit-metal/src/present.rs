//! Present: a `CAMetalLayer` drawable path.
//!
//! The window belongs to `nxkit-host`. This module takes a layer, hands out drawables and presents them with a
//! frame. A tick of the display link hands out the drawable, and `Frame::present_at` shows it at the target time of
//! the tick. [`crate::DisplayLink`] and [`crate::Pacer`] drive the ticks.

use std::ffi::c_void;
use std::ptr::NonNull;

use block2::RcBlock;
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

/// The least time in microseconds that a shown frame stays before the next one may appear. 0 is off.
static MIN_STAY_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Turns on timed presents: every frame is presented with `presentAfterMinimumDuration:` of `secs`, so the display
/// keeps the frame before it on screen at least that long. A screen that adapts its refresh rate (ProMotion) then
/// shows each frame for exactly its period, whatever rate the display runs at in that moment. `None` turns it off.
/// Metal can refuse the call on a drawable of the display link. The first refusal turns timed presents off again.
pub fn set_timed_present(secs: Option<f64>) {
    let us = secs
        .filter(|s| s.is_finite() && *s > 0.0)
        .map_or(0, |s| (s * 1e6).round() as u64);
    MIN_STAY_US.store(us, std::sync::atomic::Ordering::Relaxed);
}

/// The minimum stay of timed presents in seconds, when they are on.
pub fn timed_present() -> Option<f64> {
    match MIN_STAY_US.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        us => Some(us as f64 / 1e6),
    }
}

/// A drawable of the layer. Render into [`Drawable::texture`], then call [`Frame::present`].
pub struct Drawable {
    texture: Texture,
    raw: Retained<ProtocolObject<dyn CAMetalDrawable>>,
}

impl Drawable {
    /// Calls `f` with the time in seconds on the host clock at which the display showed the drawable, 0 when it did not
    /// show it. It must be called before the drawable is presented.
    pub fn on_presented(&self, f: impl Fn(f64) + Send + Sync + 'static) {
        let block: RcBlock<'static, fn(NonNull<ProtocolObject<dyn MTLDrawable>>), dyn Send + Sync> =
            RcBlock::new(move |d: NonNull<ProtocolObject<dyn MTLDrawable>>| {
                // SAFETY: Metal passes a valid drawable that lives for the call.
                f(unsafe { d.as_ref() }.presentedTime());
            });
        self.raw.addPresentedHandler(&block);
    }

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
