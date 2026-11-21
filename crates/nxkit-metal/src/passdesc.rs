//! The render pass descriptor that a command buffer keeps and every render pass of it fills again. The command buffer
//! (`encode`) owns it and the render pass (`render`) fills it, so it sits below both.

use objc2::rc::Retained;
use objc2_metal::MTL4RenderPassDescriptor;

/// A render pass descriptor that is filled again for every pass of a command buffer. Metal copies a descriptor when
/// an encoder starts, so one object serves all passes.
pub(crate) struct PassDescriptor {
    pub(crate) raw: Retained<MTL4RenderPassDescriptor>,
    /// The colour slots below this index may hold a texture.
    pub(crate) colors: usize,
    pub(crate) depth: bool,
    pub(crate) stencil: bool,
}

impl PassDescriptor {
    pub(crate) fn new() -> Self {
        Self {
            raw: MTL4RenderPassDescriptor::new(),
            colors: 0,
            depth: false,
            stencil: false,
        }
    }

    /// Empties the attachments that the previous pass set.
    pub(crate) fn reset(&mut self) {
        let slots = self.raw.colorAttachments();
        for i in 0..self.colors {
            // SAFETY: the index is a slot that an earlier pass set, so it is inside the attachment limit.
            let a = unsafe { slots.objectAtIndexedSubscript(i) };
            a.setTexture(None);
            a.setResolveTexture(None);
        }
        if self.depth {
            self.raw.depthAttachment().setTexture(None);
        }
        if self.stencil {
            self.raw.stencilAttachment().setTexture(None);
        }
        (self.colors, self.depth, self.stencil) = (0, false, false);
    }
}
