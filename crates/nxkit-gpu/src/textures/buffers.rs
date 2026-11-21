//! Texture buffers that a shader declares as a `texture_buffer`, as an image or a fetched texture: a Metal texture buffer over the guest bytes.
//!
//! A shader that samples a texture buffer reads it as a 2D texture of 16384-texel rows (`describe`), but an image
//! store or load of a buffer is declared as a `texture_buffer`, which Metal binds only to a texture of the
//! `textureBuffer` type. Such a texture lies over the bytes of the pool, so what a dispatch writes is in guest memory
//! at once, for the CPU and for the 2D form that a later pass samples.

use nxkit_metal::{Format, Texture};
use nxkit_nvn::resource::TextureRecord;

use super::TextureStore;
use crate::skip::Skip;

impl TextureStore {
    /// The texture buffer of `rec` in `format`, over its guest bytes, written by shaders when `write`. Fails when the store has no pools for the frame
    /// (the mirror of `NXKIT_PRIVATE=1`, or `NXKIT_TEX_INPLACE=0`), when the bytes lie in no pool buffer or when Metal
    /// refuses the offset (it must be a multiple of `minimumTextureBufferAlignmentForPixelFormat`).
    pub(crate) fn buffer_image(
        &self,
        rec: &TextureRecord,
        format: Format,
        write: bool,
    ) -> Result<Texture, Skip> {
        let pools = self.pools.as_ref().ok_or_else(|| {
            crate::tex_skip(format_args!(
                "image buffer {:?}: no pool buffers for this frame",
                rec.id
            ))
        })?;
        let (mem, offset) =
            crate::pools::locate(pools.as_ref(), rec.address.get(), rec.layout.size)?;
        Texture::buffer_over_guest(
            &self.ctx,
            mem,
            offset,
            u64::from(rec.desc.width),
            format,
            write,
        )
        .map_err(|why| {
            crate::tex_skip(format_args!(
                "image buffer {:?} {format:?} x{} at {:#x}: {why:?}",
                rec.id,
                rec.desc.width,
                rec.address.get()
            ))
        })
    }
}
