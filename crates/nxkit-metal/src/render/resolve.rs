//! Render passes that resolve multisample colour attachments into single-sample textures.

use objc2_metal::MTLStoreAction;

use super::{RenderPass, RenderPassDesc};
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result};
use crate::format::StoreAction;
use crate::passdesc::PassDescriptor;
use crate::table::ArgumentTable;
use crate::texture::Texture;

impl CommandBuffer {
    /// [`CommandBuffer::begin_render_pass_split`] with resolve textures: `resolves[i]` receives the resolved samples
    /// of colour attachment `i` when the pass ends. The attachment must be a multisample texture and the resolve
    /// texture a single-sample one of its format. The attachment's own store action still decides whether its samples
    /// are kept (`Store`: store and resolve) or dropped (`DontCare`: resolve only).
    pub fn begin_render_pass_resolved(
        &mut self,
        desc: &RenderPassDesc,
        vertex_table: &ArgumentTable,
        fragment_table: &ArgumentTable,
        resolves: &[Option<Texture>],
    ) -> Result<RenderPass<'_>> {
        self.begin_render_pass_with(desc, vertex_table, Some(fragment_table), None, resolves)
    }

    /// Sets the resolve textures of the colour attachments into the filled descriptor `pd`.
    pub(super) fn set_resolves(
        &mut self,
        pd: &PassDescriptor,
        desc: &RenderPassDesc,
        resolves: &[Option<Texture>],
    ) -> Result<()> {
        let slots = pd.raw.colorAttachments();
        for (i, r) in resolves.iter().enumerate() {
            let (Some(r), Some(Some(c))) = (r, desc.colors.get(i)) else {
                continue;
            };
            if c.texture.sample_count() < 2 || r.sample_count() != 1 {
                return Err(MetalError::Invalid(
                    "a resolve needs a multisample attachment and a single-sample resolve texture",
                ));
            }
            if !r.desc().render_target {
                return Err(MetalError::Invalid(
                    "the resolve texture is not a render target",
                ));
            }
            // SAFETY: the index is a colour slot that `fill_pass` set, so it is inside the attachment limit.
            let a = unsafe { slots.objectAtIndexedSubscript(i) };
            a.setResolveTexture(Some(r.raw()));
            a.setStoreAction(match c.store {
                StoreAction::Store => MTLStoreAction::StoreAndMultisampleResolve,
                StoreAction::DontCare => MTLStoreAction::MultisampleResolve,
            });
            self.retain(r.clone());
        }
        Ok(())
    }
}
