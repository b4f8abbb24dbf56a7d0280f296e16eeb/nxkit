//! Texture bytes between guest memory and Metal (R4).
//!
//! A texture in `GuestNewer` state goes into Metal with the block-linear deswizzle kernel before its first use in the
//! frame. A render target stays in Metal. It goes back to guest memory with the swizzle kernel when the frame has a CPU
//! read of it.

use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{
    CommandBuffer, ComputeCommand, ComputePass, Context, Deswizzler, Dependency, Format, GobOrder, GuestMemory,
    PipelineCache, Scratch, SurfaceLayout, Swizzler, Texture, TextureDesc, TextureKind, TextureRegion, Use,
};
use nxkit_nvn::format::format_info;
use nxkit_nvn::layout::Target;
use nxkit_nvn::resource::TextureRecord;

use crate::stats::Skip;
use crate::textures::Res;

/// The two kernels and the order of the guest layout.
pub(crate) struct Transfer {
    ctx: Context,
    deswizzler: Deswizzler,
    swizzler: Swizzler,
    order: GobOrder,
    gate: Mutex<()>,
}

/// The block-linear layout of a record, in blocks.
fn surface(rec: &TextureRecord, res: &Res, order: GobOrder) -> Result<SurfaceLayout, Skip> {
    if rec.layout.linear {
        return Err(Skip::Transfer);
    }
    let d = &rec.desc;
    let info = format_info(d.format).map_err(|_| Skip::Format)?;
    let (bw, bh) = (u32::from(info.block_w).max(1), u32::from(info.block_h).max(1));
    let (width, height, depth, layers) = match d.target {
        Target::D1 => (d.width, 1, 1, 1),
        Target::D1Array => (d.width, 1, 1, d.height.max(1)),
        Target::D3 => (d.width, d.height, d.depth.max(1), 1),
        Target::D2Array | Target::D2MultisampleArray => (d.width, d.height, 1, d.depth.max(1)),
        Target::CubemapArray => (d.width, d.height, 1, d.depth.max(6)),
        Target::Cubemap => (d.width, d.height, 1, 6),
        _ => (d.width, d.height, 1, 1),
    };
    Ok(SurfaceLayout {
        bytes_per_block: u32::from(info.bytes_per_block),
        width: width.div_ceil(bw),
        height: height.div_ceil(bh),
        depth,
        mip_levels: res.desc.mip_levels,
        layers,
        block_height: 1 << u32::from(rec.layout.tile_h).min(5),
        block_depth: 1 << u32::from(rec.layout.tile_d).min(5),
        order,
    })
}

fn guest_offset(rec: &TextureRecord, guest: &GuestMemory) -> Result<u64, Skip> {
    rec.address.get().checked_sub(guest.address_map().gpu_base()).ok_or(Skip::Buffer)
}

fn extent(d: &TextureDesc, level: u32) -> [u32; 3] {
    d.level_extent(level)
}

impl Transfer {
    pub(crate) fn new(ctx: &Context, cache: &PipelineCache, order: GobOrder) -> nxkit_metal::Result<Self> {
        Ok(Self {
            ctx: ctx.clone(),
            deswizzler: Deswizzler::new(ctx, cache)?,
            swizzler: Swizzler::new(ctx, cache)?,
            order,
            gate: Mutex::new(()),
        })
    }

    /// Encodes the deswizzle of all levels and layers of `rec` into `res`. The commands go into a compute pass of
    /// `cb`. The caller holds back other users of the kernel until it returns.
    pub(crate) fn upload(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        rec: &TextureRecord,
        res: &Res,
    ) -> Result<(), Skip> {
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let surf = surface(rec, res, self.order)?;
        let base = guest_offset(rec, guest)?;
        let d = res.desc;
        let compressed = matches!(d.format, Format::Bc(_) | Format::Astc { .. });
        let mut pass = cb.begin_compute_pass(self.deswizzler.table()).map_err(|_| Skip::Transfer)?;
        let r = if compressed {
            self.upload_compressed(&mut pass, scratch, guest, base, &surf, res)
        } else {
            self.upload_plain(&mut pass, scratch, guest, base, &surf, res)
        };
        pass.end();
        r
    }

    fn upload_plain(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        base: u64,
        surf: &SurfaceLayout,
        res: &Res,
    ) -> Result<(), Skip> {
        let d = res.desc;
        let slices = if d.kind == TextureKind::D3 { 1 } else { d.slice_count() };
        for slice in 0..slices {
            for level in 0..d.mip_levels {
                let region = TextureRegion::whole(level, slice, extent(&d, level));
                self.deswizzler
                    .encode_region(pass, scratch, guest, base, surf, &res.tex, &region)
                    .map_err(|_| Skip::Transfer)?;
            }
        }
        Ok(())
    }

    /// A compressed texture is deswizzled into an uncompressed texture with one texel per block and copied over.
    /// Whether Metal accepts the copy from an unsigned integer texture into a block compressed texture of the same
    /// block size is unverified.
    fn upload_compressed(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        base: u64,
        surf: &SurfaceLayout,
        res: &Res,
    ) -> Result<(), Skip> {
        let d = res.desc;
        let uint = Format::uint_for_block_bytes(surf.bytes_per_block).ok_or(Skip::Format)?;
        let staging_desc = TextureDesc {
            kind: if d.slice_count() > 1 { TextureKind::D2Array } else { TextureKind::D2 },
            width: surf.width,
            height: surf.height,
            depth: 1,
            mip_levels: d.mip_levels,
            array_layers: d.slice_count(),
            allow_views: true,
            format: uint,
            shader_read: true,
            shader_write: true,
            render_target: false,
            storage: nxkit_metal::Storage::Private,
        };
        let staging = Texture::new(&self.ctx, staging_desc).map_err(|_| Skip::Transfer)?;
        self.ctx.commit_residency();
        let layout = SurfaceLayout { depth: 1, ..*surf };
        for slice in 0..d.slice_count() {
            for level in 0..d.mip_levels {
                let size = staging_desc.level_extent(level);
                let region = TextureRegion::whole(level, slice, size);
                self.deswizzler
                    .encode_region(pass, scratch, guest, base, &layout, &staging, &region)
                    .map_err(|_| Skip::Transfer)?;
                pass.encode(&ComputeCommand::Depend(Dependency::within_pass(Use::ComputeWrite, Use::CopyRead)))
                    .map_err(|_| Skip::Transfer)?;
                pass.encode(&ComputeCommand::CopyTexture {
                    src: staging.clone(),
                    dst: res.tex.clone(),
                    src_region: region,
                    dst_level: level,
                    dst_slice: slice,
                    dst_origin: [0, 0, 0],
                })
                .map_err(|_| Skip::Transfer)?;
            }
        }
        pass.retain(staging);
        Ok(())
    }

    /// Encodes the copy of the whole texture back into guest memory, in its block-linear layout.
    pub(crate) fn copy_back(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        rec: &TextureRecord,
        res: &Res,
    ) -> Result<(), Skip> {
        let d = res.desc;
        if res.widened || res.scaled || matches!(d.format, Format::Bc(_) | Format::Astc { .. }) {
            return Err(Skip::Transfer);
        }
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let surf = surface(rec, res, self.order)?;
        let base = guest_offset(rec, guest)?;
        let mut pass = cb.begin_compute_pass(self.swizzler.table()).map_err(|_| Skip::Transfer)?;
        let mut r = pass.encode(&ComputeCommand::WaitForEarlierPasses).map_err(|_| Skip::Transfer);
        let slices = if d.kind == TextureKind::D3 { 1 } else { d.slice_count() };
        'all: for slice in 0..slices {
            for level in 0..d.mip_levels {
                if r.is_err() {
                    break 'all;
                }
                let region = TextureRegion::whole(level, slice, extent(&d, level));
                r = self
                    .swizzler
                    .encode_region(&mut pass, scratch, guest, base, &surf, &res.tex, &region)
                    .map_err(|_| Skip::Transfer);
            }
        }
        pass.end();
        r
    }
}
