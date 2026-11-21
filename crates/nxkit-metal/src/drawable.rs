//! A drawable of the layer and the clock of the display. The presenter hands drawables out, the display link hands one
//! to the hooks of each tick, and a frame presents it.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLDrawable, MTLTexture};
use objc2_quartz_core::CAMetalDrawable;

use crate::device::Context;
use crate::format::Format;
use crate::texture::{Storage, Texture, TextureDesc};

/// The block that Metal calls when a drawable was presented.
type PresentedHandler =
    RcBlock<'static, fn(NonNull<ProtocolObject<dyn MTLDrawable>>), dyn Send + Sync>;

/// The pixel format of the layer.
pub const PRESENT_FORMAT: Format = Format::Bgra8Unorm;

/// The time in seconds on the clock of `Drawable::on_presented` and `DisplayTick` (`CACurrentMediaTime`).
pub fn media_time() -> f64 {
    // SAFETY: `CACurrentMediaTime` has no preconditions. QuartzCore is linked through `objc2-quartz-core`.
    unsafe { CACurrentMediaTime() }
}

unsafe extern "C" {
    fn CACurrentMediaTime() -> f64;
}

/// A drawable of the layer. Render into [`Drawable::texture`], then call [`Frame::present`](crate::Frame::present).
pub struct Drawable {
    pub(crate) texture: Texture,
    pub(crate) raw: Retained<ProtocolObject<dyn CAMetalDrawable>>,
}

impl Drawable {
    /// Calls `f` with the time in seconds on the host clock at which the display showed the drawable, 0 when it did not
    /// show it. It must be called before the drawable is presented.
    pub fn on_presented(&self, f: impl Fn(f64) + Send + Sync + 'static) {
        let block: PresentedHandler =
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
