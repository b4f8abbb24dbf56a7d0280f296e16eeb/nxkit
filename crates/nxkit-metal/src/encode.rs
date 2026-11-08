//! The command buffer that passes are encoded into.
//!
//! The API is data driven. A caller builds [`crate::RenderCommand`] and [`crate::ComputeCommand`] values and feeds
//! them to a pass with `encode`, so the frame encoder is a loop over recorded commands. The pass checks what Metal
//! would otherwise trust, for example that a pipeline is bound before a draw.
//!
//! The GPU addresses in commands point into the guest buffer or into a scratch buffer. Both are resident.

use std::any::Any;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::MTL4CommandBuffer;

use crate::table::Keep;

pub(crate) type RawCommandBuffer = Retained<ProtocolObject<dyn MTL4CommandBuffer>>;

/// A Metal 4 command buffer that is being encoded.
pub struct CommandBuffer {
    pub(crate) raw: RawCommandBuffer,
    retained: Vec<Keep>,
    pub(crate) encode_passes: u32,
    /// The pass descriptor that every render pass of this command buffer fills again, and what it holds.
    pub(crate) pass_desc: Option<crate::render::PassDescriptor>,
}

// SAFETY: a Metal 4 command buffer may be encoded on any thread, one thread at a time. `CommandBuffer` is not `Sync`,
// so one thread owns it while it encodes. The retained values are `Keep`, which are `Send`.
unsafe impl Send for CommandBuffer {}

impl CommandBuffer {
    pub(crate) fn new(raw: RawCommandBuffer) -> Self {
        Self {
            raw,
            retained: Vec::new(),
            encode_passes: 0,
            pass_desc: None,
        }
    }

    pub(crate) fn raw(&self) -> &RawCommandBuffer {
        &self.raw
    }

    pub(crate) fn take_retained(&mut self) -> Vec<Keep> {
        std::mem::take(&mut self.retained)
    }

    /// Keeps a value alive until the GPU finished the command buffer.
    pub fn retain(&mut self, v: impl Any) {
        self.retained.push(Keep::new(v));
    }
}
