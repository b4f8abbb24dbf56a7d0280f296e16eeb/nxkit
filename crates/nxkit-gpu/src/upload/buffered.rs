//! Uploads through a staging buffer, for the textures that the deswizzle kernel cannot write.
//!
//! The kernel writes a texture through an unsigned integer view of the block size. Metal has no such view of a
//! compressed texture (a view between compressed and uncompressed formats aborts) nor of a depth or stencil texture, and
//! those textures are not shader writable. Their blocks are deswizzled into a buffer instead and the buffer is copied
//! into the texture with a blit, which Metal allows for compressed formats, `Depth16Unorm`, `Depth32Float` and
//! `Stencil8`. `Depth32Float_Stencil8` would need a blit per aspect with another byte layout than the guest one, and NVN's
//! 24 bit depth formats are widened (their bytes differ anyway), so they have no upload.

use std::sync::{Arc, PoisonError};

use nxkit_metal::{
    ComputeCommand, ComputePass, DataBuffer, Dependency, DeswizzleParams, Format, GuestMemory,
    Scratch, SurfaceLayout, TextureKind, TextureRegion, Use,
};
use nxkit_nvn::format::format_info;
use nxkit_nvn::resource::TextureRecord;

use super::{Transfer, staging};
use crate::stats::Skip;
use crate::textures::Res;

/// True for a format whose upload goes through a buffer: compressed formats and the depth and stencil formats that a
/// buffer-to-texture blit can fill with the guest bytes as they are.
pub(super) fn via_buffer(f: Format) -> bool {
    f.is_compressed()
        || matches!(
            f,
            Format::Depth16Unorm | Format::Depth32Float | Format::Stencil8
        )
}

/// The extent in blocks of each level of a texture of `extent` texels per level, with blocks of `block` texels. A
/// compressed mip chain keeps whole blocks below the block size, so each level is rounded up on its own.
pub(super) fn level_blocks(extents: &[[u32; 3]], block: (u32, u32)) -> Vec<[u32; 3]> {
    extents
        .iter()
        .map(|[w, h, d]| [w.div_ceil(block.0), h.div_ceil(block.1), (*d).max(1)])
        .collect()
}

impl Transfer {
    /// Deswizzles every level and slice of `rec` into one pooled staging buffer and copies it into `res`. A 2D level
    /// takes the exact block extent of NVN's level (also the tail levels below the block size), a 3D level all its depth
    /// slices at once.
    pub(super) fn upload_buffered(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        base: u64,
        surf: &SurfaceLayout,
        (rec, res): (&TextureRecord, &Res),
    ) -> Result<(), Skip> {
        let d = res.desc;
        let info = format_info(rec.desc.format).map_err(|_| Skip::Format)?;
        let three_d = d.kind == TextureKind::D3;
        let extents: Vec<[u32; 3]> = (0..d.mip_levels)
            .map(|level| {
                let [w, h, z] = d.level_extent(level);
                [w, h, if three_d { z } else { 1 }]
            })
            .collect();
        let sizes = level_blocks(
            &extents,
            (
                u32::from(info.block_w).max(1),
                u32::from(info.block_h).max(1),
            ),
        );
        if three_d {
            // The kernel walks the surface layout, whose level extents must be the ones of the texture.
            for (level, s) in sizes.iter().enumerate() {
                let l = surf.level(level as u32).map_err(|_| Skip::Transfer)?;
                if [l.width, l.height, l.depth] != *s {
                    return Err(Skip::Transfer);
                }
            }
        }
        let bytes: Vec<u64> = sizes
            .iter()
            .map(|s| {
                s.iter().map(|v| u64::from(*v)).product::<u64>() * u64::from(surf.bytes_per_block)
            })
            .collect();
        let (offsets, total) = staging::level_offsets(&bytes);
        if total == 0 {
            return Ok(());
        }
        // Every byte of a level range is written by the deswizzle before the copy reads it, so a reused buffer needs
        // no clearing and a new one is not zero filled by the CPU.
        let staging = self
            .staging
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .acquire(total, |len| {
                DataBuffer::new_unfilled(&self.ctx, len).ok().map(Arc::new)
            })
            .ok_or(Skip::Transfer)?;
        // The command buffer holds the buffer from here on, also when an error cuts the encoding short, so the pool
        // never hands it out while commands that name it can still run.
        pass.retain(staging.clone());
        let slices = if three_d { 1 } else { d.slice_count() };
        for level in 0..d.mip_levels {
            let size = sizes[level as usize];
            let at = offsets[level as usize];
            for slice in 0..slices {
                // The slices of a level share its range of the buffer.
                if slice != 0 {
                    pass.encode(&ComputeCommand::Depend(Dependency::within_pass(
                        Use::CopyRead,
                        Use::ComputeWrite,
                    )))
                    .map_err(|_| Skip::Transfer)?;
                }
                if three_d {
                    self.deswizzler
                        .encode_level_to_buffer(
                            pass,
                            scratch,
                            guest,
                            base,
                            surf,
                            level,
                            (&staging, at),
                        )
                        .map_err(|_| Skip::Transfer)?;
                } else {
                    let (tile_h, _) = rec.desc.level_tiles(level).map_err(|_| Skip::Transfer)?;
                    let params = DeswizzleParams {
                        bytes_per_block: surf.bytes_per_block,
                        width: size[0],
                        height: size[1],
                        block_height: 1 << tile_h,
                        pitch: 0,
                        order: surf.order,
                    };
                    let offset = rec.desc.level_offset(level).map_err(|_| Skip::Transfer)?;
                    self.deswizzler
                        .encode_to_buffer(
                            pass,
                            scratch,
                            guest,
                            base + u64::from(slice) * rec.layout.layer_size + offset,
                            (&staging, at),
                            &params,
                        )
                        .map_err(|_| Skip::Transfer)?;
                }
                pass.encode(&ComputeCommand::Depend(Dependency::within_pass(
                    Use::ComputeWrite,
                    Use::CopyRead,
                )))
                .map_err(|_| Skip::Transfer)?;
                pass.encode(&ComputeCommand::CopyBufferToTexture {
                    buffer: staging.clone(),
                    offset: at,
                    bytes_per_row: size[0] * surf.bytes_per_block,
                    texture: res.tex.clone(),
                    region: TextureRegion::whole(level, slice, extents[level as usize]),
                })
                .map_err(|_| Skip::Transfer)?;
            }
        }
        Ok(())
    }
}
