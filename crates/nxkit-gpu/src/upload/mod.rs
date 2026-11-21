//! Texture bytes between guest memory and Metal (R4).
//!
//! A texture in `GuestNewer` state goes into Metal with the block-linear deswizzle kernel before its first use in the
//! frame. A render target stays in Metal. It goes back to guest memory with the swizzle kernel when the frame has a CPU
//! read of it, or at the end of a frame that wrote it when its storage lies in a pool the CPU can access (`copyback`).

mod buffered;
mod dirty;
#[cfg(test)]
mod layout_tests;
mod sparse;
mod staging;
mod surface;
#[cfg(test)]
mod tests;

pub(crate) use self::dirty::Dirty;
pub(crate) use self::sparse::virtual_window;
use self::surface::surface;

use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{
    CommandBuffer, ComputeCommand, ComputePass, Context, DataBuffer, Deswizzler, Format, GobOrder,
    GuestMemory, PipelineCache, Scratch, SurfaceLayout, Swizzler, TextureDesc, TextureKind,
    TextureRegion,
};
use nxkit_nvn::layout::Target;
use nxkit_nvn::resource::TextureRecord;

use crate::guestpools::GuestPools;
use crate::pools::{Backing, PoolSnapshot};
use crate::scaled::ScaledUploads;
use crate::skip::Skip;
use crate::textures::Res;

/// The two kernels and the order of the guest layout.
pub(crate) struct Transfer {
    ctx: Context,
    deswizzler: Deswizzler,
    swizzler: Swizzler,
    order: GobOrder,
    gate: Mutex<()>,
    /// The staging buffers of the uploads through a buffer (compressed, depth and stencil textures).
    staging: Mutex<staging::Pool<DataBuffer>>,
    /// The staging pool and the queued scalings of uploads into scaled textures.
    scaled: Mutex<ScaledUploads>,
}

/// Counts a failed upload for the health report and prints it under the `upload` category (at most 300 lines).
fn upload_failed(what: std::fmt::Arguments<'_>) {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::UploadFailed, what);
    if crate::diag("upload") && crate::diag::first(&SHOWN, 300) {
        eprintln!("DIAG upload failed {what}");
    }
}

pub(crate) use crate::pools::locate;

use crate::textures::row_pitch;

/// The regions of a pitch-linear texture that hold guest bytes, each with the offset of its first byte from the first
/// byte of the texture. A texture buffer of several rows ends in a short row: its full rows are one region and the
/// texels of the last row another, so a copy never reaches past the bytes of the buffer.
fn linear_parts(rec: &TextureRecord, d: &TextureDesc) -> Vec<(TextureRegion, u64)> {
    let whole = TextureRegion::whole(0, 0, extent(d, 0));
    if rec.desc.target != Target::Buffer || d.height <= 1 {
        return vec![(whole, 0)];
    }
    let full = d.height - 1;
    let last = rec.desc.width - full * d.width;
    let mut parts = vec![(
        TextureRegion {
            size: [d.width, full, 1],
            ..whole
        },
        0,
    )];
    if last > 0 {
        parts.push((
            TextureRegion {
                origin: [0, full, 0],
                size: [last, 1, 1],
                ..whole
            },
            u64::from(full) * u64::from(row_pitch(rec)),
        ));
    }
    parts
}

/// The pool buffer that holds all bytes of the texture of `rec` and the offset of its first byte in it.
fn guest_offset<'p, B: Backing>(
    rec: &TextureRecord,
    pools: &'p PoolSnapshot<B>,
) -> Result<(&'p B, u64), Skip> {
    locate(pools, rec.address.get(), rec.layout.size)
}

/// `NXKIT_DIAG=tiny`: the guest bytes of a texture of at most 256 bytes, as the upload reads them.
fn tiny_upload(rec: &TextureRecord, res: &Res, guest: &Arc<GuestMemory>, base: u64) {
    let size = rec.layout.size;
    if size == 0 || size > crate::diag::TINY_MAX || !crate::diag::tiny_line() {
        return;
    }
    let mut buf = vec![0u8; size as usize];
    if guest.read(base, &mut buf).is_err() {
        return;
    }
    let d = res.desc;
    eprintln!(
        "DIAG tiny upload {:?} {:?} {}x{} at 0x{:x} {}",
        rec.id,
        d.format,
        d.width,
        d.height,
        rec.address.get(),
        crate::diag::tiny_words(&buf)
    );
}

fn extent(d: &TextureDesc, level: u32) -> [u32; 3] {
    d.level_extent(level)
}

impl Transfer {
    pub(crate) fn new(
        ctx: &Context,
        cache: &PipelineCache,
        order: GobOrder,
    ) -> nxkit_metal::Result<Self> {
        Ok(Self {
            ctx: ctx.clone(),
            deswizzler: Deswizzler::new(ctx, cache)?,
            swizzler: Swizzler::new(ctx, cache)?,
            order,
            gate: Mutex::new(()),
            staging: Mutex::new(staging::Pool::new()),
            scaled: Mutex::new(ScaledUploads::new(ctx, cache)?),
        })
    }

    /// The buffer that holds the guest bytes of `rec` and the offset of the first byte in it. A texture of a virtual
    /// pool is gathered through the page table into a buffer of its own (`sparse`).
    fn source(
        &self,
        rec: &TextureRecord,
        pools: &GuestPools,
    ) -> Result<(Arc<GuestMemory>, u64), Skip> {
        if let Some(tables) = sparse::tables_of(rec, pools) {
            return Ok((sparse::gather(&self.ctx, tables, rec, pools)?, 0));
        }
        let (guest, base) = guest_offset(rec, pools)?;
        Ok((guest.clone(), base))
    }

    /// Starts a frame of scaled uploads.
    pub(crate) fn begin_frame(&self) {
        self.scaled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .begin_frame();
    }

    /// Uploads the guest data of `rec` into the scaled texture `res`: the plain upload fills a staging texture of the
    /// native size, and the scaling is queued for [`Transfer::flush_scaled`]. Counts as lost what it cannot do.
    pub(crate) fn upload_scaled(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        pools: &GuestPools,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        if !ScaledUploads::supports(res) {
            nxkit_nvn::diag::example(
                nxkit_nvn::diag::Reason::UploadScaledLost,
                format_args!(
                    "{:?} {:?} {}x{}: no scaling kernel",
                    res.desc.format, res.desc.kind, res.desc.width, res.desc.height
                ),
            );
            return Err(Skip::Transfer);
        }
        let staging = self
            .scaled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .staging(rec, res)?;
        self.upload(cb, scratch, pools, rec, &staging, wait)?;
        cb.retain(staging.tex.clone());
        self.scaled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queue(&staging, res);
        Ok(())
    }

    /// Encodes the scalings of this frame's scaled uploads. Returns the textures scaled and the textures lost.
    pub(crate) fn flush_scaled(&self, cb: &mut CommandBuffer) -> (u32, u32) {
        self.scaled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush(cb)
    }

    /// Encodes the deswizzle of all levels and layers of `rec` into `res`. The commands go into a compute pass of
    /// `cb`. The caller holds back other users of the kernel until it returns.
    pub(crate) fn upload(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        pools: &GuestPools,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if rec.layout.linear {
            return self.upload_linear(cb, pools, rec, res, wait);
        }
        let r = self.upload_block_linear(cb, scratch, pools, rec, res, wait);
        if let Err(why) = r {
            let d = res.desc;
            upload_failed(format_args!(
                "{:?} {:?} {}x{}x{} levels {} layers {}: {why:?}",
                d.format, d.kind, d.width, d.height, d.depth, d.mip_levels, d.array_layers
            ));
        }
        r
    }

    fn upload_block_linear(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        pools: &GuestPools,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        let surf = surface(rec, res, self.order)?;
        let (guest, base) = self.source(rec, pools)?;
        let guest = &guest;
        let d = res.desc;
        tiny_upload(rec, res, guest, base);
        let buffered = buffered::via_buffer(d.format);
        let mut pass = cb
            .begin_compute_pass(self.deswizzler.table())
            .map_err(|_| Skip::Transfer)?;
        if wait && pass.encode(&ComputeCommand::WaitForEarlierPasses).is_err() {
            pass.end();
            return Err(Skip::Transfer);
        }
        let r = if buffered {
            self.upload_buffered(&mut pass, scratch, guest, base, &surf, (rec, res))
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
        let slices = if d.kind == TextureKind::D3 {
            1
        } else {
            d.slice_count()
        };
        for slice in 0..slices {
            for level in 0..d.mip_levels {
                let region = TextureRegion::whole(level, slice, extent(&d, level));
                self.deswizzler
                    .encode_region(pass, scratch, guest, base, surf, &res.tex, &region)
                    .map_err(|e| {
                        // The caller counts the failure. This keeps what the kernel said as an example.
                        nxkit_nvn::diag::example(
                            nxkit_nvn::diag::Reason::UploadFailed,
                            format_args!(
                                "{:?} {}x{} level {level} slice {slice}: {e}",
                                d.format, d.width, d.height
                            ),
                        );
                        Skip::Transfer
                    })?;
            }
        }
        Ok(())
    }

    /// A pitch-linear texture is a plain copy of its rows: guest memory holds the texels row by row, `stride` bytes
    /// apart. NVN linear textures have one level and one layer, which is all this copies.
    fn upload_linear(
        &self,
        cb: &mut CommandBuffer,
        pools: &GuestPools,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        let d = res.desc;
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if d.kind == TextureKind::D3
            || d.mip_levels != 1
            || d.array_layers != 1
            || row_pitch(rec) == 0
        {
            let what = format_args!(
                "{:?} {:?} {}x{}x{} levels {} layers {} stride {}",
                d.format,
                d.kind,
                d.width,
                d.height,
                d.depth,
                d.mip_levels,
                d.array_layers,
                row_pitch(rec)
            );
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::UploadLinearRefused, what);
            if crate::diag("upload") && crate::diag::first(&SHOWN, 300) {
                eprintln!("DIAG upload linear refused {what}");
            }
            return Err(Skip::Transfer);
        }
        if crate::diag("upload") && crate::diag::first(&SHOWN, 300) {
            eprintln!(
                "DIAG upload linear {:?} {}x{} stride {}",
                d.format,
                d.width,
                d.height,
                row_pitch(rec)
            );
        }
        let r = self.copy_linear(cb, pools, rec, res, wait);
        if let Err(why) = r {
            upload_failed(format_args!(
                "linear {:?} {}x{} stride {}: {why:?}",
                d.format,
                d.width,
                d.height,
                row_pitch(rec)
            ));
        }
        r
    }

    fn copy_linear(
        &self,
        cb: &mut CommandBuffer,
        pools: &GuestPools,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        let d = res.desc;
        let (guest, base) = self.source(rec, pools)?;
        tiny_upload(rec, res, &guest, base);
        let mut pass = cb
            .begin_compute_pass(self.deswizzler.table())
            .map_err(|_| Skip::Transfer)?;
        if wait && pass.encode(&ComputeCommand::WaitForEarlierPasses).is_err() {
            pass.end();
            return Err(Skip::Transfer);
        }
        let r = linear_parts(rec, &d)
            .into_iter()
            .try_for_each(|(region, off)| {
                pass.encode(&ComputeCommand::CopyGuestToTexture {
                    guest: guest.clone(),
                    offset: base + off,
                    bytes_per_row: row_pitch(rec),
                    bytes_per_image: 0,
                    texture: res.tex.clone(),
                    region,
                })
                .map_err(|_| Skip::Transfer)
            });
        pass.end();
        r
    }

    /// Encodes the copy of the whole texture back into guest memory, in its block-linear layout. `end` encodes the
    /// last command of the compute pass (the pass profile's timestamp).
    pub(crate) fn copy_back(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        pools: &GuestPools,
        rec: &TextureRecord,
        res: &Res,
        end: Option<&dyn Fn(&mut ComputePass<'_>)>,
    ) -> Result<(), Skip> {
        let d = res.desc;
        if res.widened || res.scaled || matches!(d.format, Format::Bc(_) | Format::Astc { .. }) {
            return Err(Skip::Transfer);
        }
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let surf = surface(rec, res, self.order)?;
        let (guest, base) = guest_offset(rec, pools)?;
        let mut pass = cb
            .begin_compute_pass(self.swizzler.table())
            .map_err(|_| Skip::Transfer)?;
        let mut r = pass
            .encode(&ComputeCommand::WaitForEarlierPasses)
            .map_err(|_| Skip::Transfer);
        let slices = if d.kind == TextureKind::D3 {
            1
        } else {
            d.slice_count()
        };
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
        if let Some(end) = end {
            end(&mut pass);
        }
        pass.end();
        r
    }

    /// Encodes the copy-back of the parts of `items` that the frame wrote into guest memory, all in one compute pass
    /// that waits for the earlier work of the queue (the passes of this frame and of the frames before). A
    /// block-linear texture goes through the swizzle kernel, a pitch-linear one is a plain copy of its rows. The
    /// result of each item is the number of texel bytes it copied, or why it was refused. A refused item may have
    /// written part of its storage.
    #[cfg(test)]
    pub(crate) fn copy_back_many(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        pools: &GuestPools,
        items: &[(&TextureRecord, &Res, &Dirty)],
    ) -> Vec<Result<u64, Skip>> {
        self.copy_back_many_with(cb, scratch, pools, items, None)
    }

    /// [`Transfer::copy_back_many`], where `end` encodes the last command of the compute pass.
    pub(crate) fn copy_back_many_with(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        pools: &GuestPools,
        items: &[(&TextureRecord, &Res, &Dirty)],
        end: Option<&dyn Fn(&mut ComputePass<'_>)>,
    ) -> Vec<Result<u64, Skip>> {
        if items.is_empty() {
            return Vec::new();
        }
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let Ok(mut pass) = cb.begin_compute_pass(self.swizzler.table()) else {
            return vec![Err(Skip::Transfer); items.len()];
        };
        if pass.encode(&ComputeCommand::WaitForEarlierPasses).is_err() {
            pass.end();
            return vec![Err(Skip::Transfer); items.len()];
        }
        let out = items
            .iter()
            .map(|(rec, res, dirty)| self.copy_back_one(&mut pass, scratch, pools, rec, res, dirty))
            .collect();
        if let Some(end) = end {
            end(&mut pass);
        }
        pass.end();
        out
    }

    fn copy_back_one(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        pools: &GuestPools,
        rec: &TextureRecord,
        res: &Res,
        dirty: &Dirty,
    ) -> Result<u64, Skip> {
        let d = res.desc;
        if res.widened
            || res.scaled
            || d.format.is_compressed()
            || d.format.is_depth()
            || d.format.has_stencil()
        {
            return Err(Skip::Transfer);
        }
        let (guest, base) = guest_offset(rec, pools)?;
        let bpp = u64::from(d.format.block_bytes());
        let bytes = |level: u32| {
            let [w, h, z] = extent(&d, level);
            u64::from(w) * u64::from(h) * u64::from(z) * bpp
        };
        if rec.layout.linear {
            // NVN linear textures have one level and one layer (see `upload_linear`).
            if d.kind == TextureKind::D3
                || d.mip_levels != 1
                || d.array_layers != 1
                || row_pitch(rec) == 0
            {
                return Err(Skip::Transfer);
            }
            for (region, off) in linear_parts(rec, &d) {
                pass.encode(&ComputeCommand::CopyTextureRegionToGuest {
                    texture: res.tex.clone(),
                    region,
                    guest: guest.clone(),
                    offset: base + off,
                    bytes_per_row: row_pitch(rec),
                    bytes_per_image: 0,
                })
                .map_err(|_| Skip::Transfer)?;
            }
            return Ok(rec.layout.size.min(bytes(0)));
        }
        let surf = surface(rec, res, self.order)?;
        let slices = if d.kind == TextureKind::D3 {
            1
        } else {
            d.slice_count()
        };
        let mut total = 0;
        for (level, slice) in dirty.parts(d.mip_levels, slices) {
            let region = TextureRegion::whole(level, slice, extent(&d, level));
            self.swizzler
                .encode_region(pass, scratch, guest, base, &surf, &res.tex, &region)
                .map_err(|_| Skip::Transfer)?;
            total += bytes(level);
        }
        Ok(total)
    }
}
