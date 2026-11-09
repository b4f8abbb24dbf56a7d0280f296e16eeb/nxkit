//! The copies of a compute pass between textures, buffers and guest memory.

use std::sync::Arc;

use objc2::runtime::ProtocolObject;
use objc2_metal::{MTL4ComputeCommandEncoder as _, MTLOrigin, MTLSize};

use crate::compute::{ComputePass, size3};
use crate::databuf::DataBuffer;
use crate::error::{MetalError, Result};
use crate::guest::GuestMemory;
use crate::scratch::Scratch;
use crate::texture::{Texture, TextureRegion};

/// Checks a box of a texture against a buffer layout of offset, bytes per row and bytes per depth slice. Returns the
/// bytes that the box spans in the buffer and the bytes per image to give Metal.
fn box_layout(
    texture: &Texture,
    region: &TextureRegion,
    (offset, bytes_per_row, bytes_per_image): (u64, u32, u32),
) -> Result<(u64, usize)> {
    let d = texture.desc();
    region.check(&d)?;
    let format = d.format;
    if format.is_depth() {
        return Err(MetalError::Invalid("a depth texture needs blit options"));
    }
    let (bw, bh) = format.block_dim();
    let block = u64::from(format.block_bytes());
    let rows = u64::from(region.size[1].div_ceil(bh));
    let row = u64::from(region.size[0].div_ceil(bw)) * block;
    let (bpr, bpi) = (u64::from(bytes_per_row), u64::from(bytes_per_image));
    let depth = u64::from(region.size[2]);
    if !region.origin[0].is_multiple_of(bw)
        || !region.origin[1].is_multiple_of(bh)
        || bpr < row
        || bpr % block != 0
        || offset % block != 0
        || (depth > 1 && bpi < bpr * rows)
    {
        return Err(MetalError::Invalid(
            "the buffer image range, row pitch or box alignment is invalid",
        ));
    }
    Ok((bpi * (depth - 1) + bpr * rows, bpi.max(bpr * rows) as usize))
}

impl ComputePass<'_> {
    /// Blits a box of one level and slice of `texture` (or a box of a 3D texture) to `buffer` with `to_buffer`, or
    /// from it without. The buffer holds rows of blocks `bytes_per_row` apart from `offset` on, the depth slices of a
    /// 3D box tightly after each other. Depth and stencil textures are refused (they need blit options).
    pub fn copy_buffer_box(
        &mut self,
        to_buffer: bool,
        texture: &Texture,
        region: &TextureRegion,
        buffer: &Arc<DataBuffer>,
        (offset, bytes_per_row): (u64, u32),
    ) -> Result<()> {
        let rows = region.size[1].div_ceil(texture.desc().format.block_dim().1);
        let image = bytes_per_row
            .checked_mul(rows)
            .ok_or(MetalError::Invalid("the image of the box is too large"))?;
        let layout = (offset, bytes_per_row, image);
        let (bytes, per_image) = box_layout(texture, region, layout)?;
        if offset
            .checked_add(bytes)
            .is_none_or(|end| end > buffer.len())
        {
            return Err(MetalError::Invalid("the buffer is too small for the box"));
        }
        // SAFETY: `box_layout` checked the box and the layout, and the whole range of the box is inside the buffer
        // (checked above). The buffer is retained below until the GPU is done.
        unsafe { self.blit_box(to_buffer, texture, region, &buffer.raw, layout, per_image) };
        self.cb.retain(buffer.clone());
        Ok(())
    }

    /// Blits a box between a texture and guest memory. `to_guest` picks the direction. `layout` is the offset, the
    /// bytes per row and the bytes per depth slice in guest memory.
    pub(crate) fn guest_texture_box(
        &mut self,
        to_guest: bool,
        texture: &Texture,
        region: &TextureRegion,
        guest: &Arc<GuestMemory>,
        layout: (u64, u32, u32),
    ) -> Result<()> {
        let (bytes, per_image) = box_layout(texture, region, layout)?;
        guest.gpu_address_of_offset(layout.0, bytes)?;
        // SAFETY: `box_layout` checked the box and the layout, and the guest range `[offset, offset + bytes)` is
        // inside the guest buffer (checked above). The guest memory is retained below until the GPU is done.
        unsafe { self.blit_box(to_guest, texture, region, guest.buffer(), layout, per_image) };
        self.cb.retain(guest.clone());
        Ok(())
    }

    /// Copies bytes that lie in `scratch` into a box of a texture. `address` is the GPU address that
    /// [`Scratch::push`] returned. The rows are `bytes_per_row` apart and the box must be one slice deep, so a caller
    /// that fills a 3D or array texture calls this once per slice with the same bytes.
    ///
    /// The scratch buffer is resident and not reused before the batch of the pass is done.
    pub fn copy_scratch_to_texture(
        &mut self,
        scratch: &Scratch,
        address: u64,
        bytes_per_row: u32,
        texture: &Texture,
        region: &TextureRegion,
    ) -> Result<()> {
        if region.size[2] != 1 {
            return Err(MetalError::Invalid("a scratch copy is one slice deep"));
        }
        // Scratch blocks start at a multiple of `SCRATCH_ALIGN`, which is a multiple of every block size.
        let (bytes, per_image) = box_layout(texture, region, (0, bytes_per_row, 0))?;
        let (buffer, offset) = scratch.locate(address, bytes).ok_or(MetalError::Invalid(
            "the scratch range is not in one buffer",
        ))?;
        let layout = (offset, bytes_per_row, 0);
        // SAFETY: `box_layout` checked the box and the layout against the texture, and `locate` checked that
        // `[offset, offset + bytes)` is inside `buffer`. The texture is retained below and the buffer is owned by the
        // scratch, which the batch keeps until the GPU is done.
        unsafe { self.blit_box(false, texture, region, &buffer, layout, per_image) };
        Ok(())
    }

    /// Encodes the blit of a box.
    ///
    /// # Safety
    ///
    /// The box must be inside the texture, the layout must pass [`box_layout`], and the bytes that it names must be
    /// inside `buffer`. `buffer` must stay alive until the GPU is done.
    unsafe fn blit_box(
        &mut self,
        to_buffer: bool,
        texture: &Texture,
        region: &TextureRegion,
        buffer: &ProtocolObject<dyn objc2_metal::MTLBuffer>,
        (offset, bytes_per_row, _): (u64, u32, u32),
        per_image: usize,
    ) {
        let origin = MTLOrigin {
            x: region.origin[0] as usize,
            y: region.origin[1] as usize,
            z: region.origin[2] as usize,
        };
        // SAFETY: the contract of this function.
        unsafe {
            if to_buffer {
                self.enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    texture.raw(), region.slice as usize, region.level as usize, origin, size3(region.size),
                    buffer, offset as usize, bytes_per_row as usize, per_image);
            } else {
                self.enc.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                    buffer, offset as usize, bytes_per_row as usize, per_image,
                    size3(region.size), texture.raw(), region.slice as usize, region.level as usize, origin);
            }
        }
        self.cb.retain(texture.clone());
    }

    pub(crate) fn copy_to_guest(
        &mut self,
        tex: &Texture,
        guest: &Arc<GuestMemory>,
        offset: u64,
        bpr: u32,
    ) -> Result<()> {
        let d = tex.desc();
        let row = u64::from(d.width) * u64::from(d.format.bytes_per_pixel());
        if u64::from(bpr) < row || u64::from(bpr) % u64::from(d.format.bytes_per_pixel()) != 0 {
            return Err(MetalError::Invalid(
                "bytes per row is too small or not a whole number of pixels",
            ));
        }
        let bytes = u64::from(bpr) * u64::from(d.height);
        guest.gpu_address_of_offset(offset, bytes)?;
        // SAFETY: the destination range is inside the guest buffer (checked above), the source is the whole
        // level 0 of a texture that outlives the pass (`retain` below).
        unsafe {
            self.enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                tex.raw(),
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize { width: d.width as usize, height: d.height as usize, depth: 1 },
                guest.buffer(),
                offset as usize,
                bpr as usize,
                bytes as usize,
            );
        }
        self.cb.retain(tex.clone());
        self.cb.retain(guest.clone());
        Ok(())
    }
}
