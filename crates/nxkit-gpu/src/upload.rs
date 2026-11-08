//! Texture bytes between guest memory and Metal (R4).
//!
//! A texture in `GuestNewer` state goes into Metal with the block-linear deswizzle kernel before its first use in the
//! frame. A render target stays in Metal. It goes back to guest memory with the swizzle kernel when the frame has a CPU
//! read of it, or at the end of a frame that wrote it when its storage lies in a pool the CPU can access (`copyback`).

use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{
    CommandBuffer, ComputeCommand, ComputePass, Context, DataBuffer, Dependency, DeswizzleParams,
    Deswizzler, Format, GobOrder, GuestMemory, PipelineCache, Scratch, SurfaceLayout, Swizzler,
    TextureDesc, TextureKind, TextureRegion, Use,
};
use nxkit_nvn::format::format_info;
use nxkit_nvn::layout::Target;
use nxkit_nvn::resource::TextureRecord;

use crate::scaled::ScaledUploads;
use crate::stats::Skip;
use crate::textures::Res;

/// The two kernels and the order of the guest layout.
pub(crate) struct Transfer {
    ctx: Context,
    deswizzler: Deswizzler,
    swizzler: Swizzler,
    order: GobOrder,
    gate: Mutex<()>,
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

/// The block-linear layout of a record, in blocks.
fn surface(rec: &TextureRecord, res: &Res, order: GobOrder) -> Result<SurfaceLayout, Skip> {
    if rec.layout.linear {
        return Err(Skip::Transfer);
    }
    let d = &rec.desc;
    let info = format_info(d.format).map_err(|_| Skip::Format)?;
    let (bw, bh) = (
        u32::from(info.block_w).max(1),
        u32::from(info.block_h).max(1),
    );
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
    rec.address
        .get()
        .checked_sub(guest.address_map().gpu_base())
        .ok_or(Skip::Buffer)
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
            scaled: Mutex::new(ScaledUploads::new(ctx, cache)?),
        })
    }

    /// Starts a frame of scaled uploads.
    pub(crate) fn begin_frame(&self) {
        self.scaled.lock().unwrap_or_else(PoisonError::into_inner).begin_frame();
    }

    /// Uploads the guest data of `rec` into the scaled texture `res`: the plain upload fills a staging texture of the
    /// native size, and the scaling is queued for [`Transfer::flush_scaled`]. Counts as lost what it cannot do.
    pub(crate) fn upload_scaled(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        if !ScaledUploads::supports(res) {
            nxkit_nvn::diag::example(
                nxkit_nvn::diag::Reason::UploadScaledLost,
                format_args!("{:?} {:?} {}x{}: no scaling kernel", res.desc.format, res.desc.kind, res.desc.width, res.desc.height),
            );
            return Err(Skip::Transfer);
        }
        let staging = self.scaled.lock().unwrap_or_else(PoisonError::into_inner).staging(rec, res)?;
        self.upload(cb, scratch, guest, rec, &staging, wait)?;
        cb.retain(staging.tex.clone());
        self.scaled.lock().unwrap_or_else(PoisonError::into_inner).queue(&staging, res);
        Ok(())
    }

    /// Encodes the scalings of this frame's scaled uploads. Returns the textures scaled and the textures lost.
    pub(crate) fn flush_scaled(&self, cb: &mut CommandBuffer) -> (u32, u32) {
        self.scaled.lock().unwrap_or_else(PoisonError::into_inner).flush(cb)
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
        wait: bool,
    ) -> Result<(), Skip> {
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if rec.layout.linear {
            return self.upload_linear(cb, guest, rec, res, wait);
        }
        let r = self.upload_block_linear(cb, scratch, guest, rec, res, wait);
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
        guest: &Arc<GuestMemory>,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        let surf = surface(rec, res, self.order)?;
        let base = guest_offset(rec, guest)?;
        let d = res.desc;
        let compressed = matches!(d.format, Format::Bc(_) | Format::Astc { .. });
        let mut pass = cb
            .begin_compute_pass(self.deswizzler.table())
            .map_err(|_| Skip::Transfer)?;
        if wait && pass.encode(&ComputeCommand::WaitForEarlierPasses).is_err() {
            pass.end();
            return Err(Skip::Transfer);
        }
        let r = if compressed {
            self.upload_compressed(&mut pass, scratch, guest, base, &surf, rec, res)
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
                            format_args!("{:?} {}x{} level {level} slice {slice}: {e}", d.format, d.width, d.height),
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
        guest: &Arc<GuestMemory>,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        let d = res.desc;
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if d.kind == TextureKind::D3 || d.mip_levels != 1 || d.array_layers != 1 || rec.desc.stride == 0 {
            let what = format_args!(
                "{:?} {:?} {}x{}x{} levels {} layers {} stride {}",
                d.format, d.kind, d.width, d.height, d.depth, d.mip_levels, d.array_layers, rec.desc.stride
            );
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::UploadLinearRefused, what);
            if crate::diag("upload") && crate::diag::first(&SHOWN, 300) {
                eprintln!("DIAG upload linear refused {what}");
            }
            return Err(Skip::Transfer);
        }
        if crate::diag("upload") && crate::diag::first(&SHOWN, 300) {
            eprintln!("DIAG upload linear {:?} {}x{} stride {}", d.format, d.width, d.height, rec.desc.stride);
        }
        let r = self.copy_linear(cb, guest, rec, res, wait);
        if let Err(why) = r {
            upload_failed(format_args!("linear {:?} {}x{} stride {}: {why:?}", d.format, d.width, d.height, rec.desc.stride));
        }
        r
    }

    fn copy_linear(
        &self,
        cb: &mut CommandBuffer,
        guest: &Arc<GuestMemory>,
        rec: &TextureRecord,
        res: &Res,
        wait: bool,
    ) -> Result<(), Skip> {
        let d = res.desc;
        let base = guest_offset(rec, guest)?;
        let mut pass = cb
            .begin_compute_pass(self.deswizzler.table())
            .map_err(|_| Skip::Transfer)?;
        if wait && pass.encode(&ComputeCommand::WaitForEarlierPasses).is_err() {
            pass.end();
            return Err(Skip::Transfer);
        }
        let r = pass
            .encode(&ComputeCommand::CopyGuestToTexture {
                guest: guest.clone(),
                offset: base,
                bytes_per_row: rec.desc.stride,
                bytes_per_image: 0,
                texture: res.tex.clone(),
                region: TextureRegion::whole(0, 0, extent(&d, 0)),
            })
            .map_err(|_| Skip::Transfer);
        pass.end();
        r
    }

    /// Compressed blocks are deswizzled into a buffer and copied into the texture. Metal does not support a direct
    /// integer-texture to compressed-texture copy. Each mip uses its exact block extent, including the tail levels.
    fn upload_compressed(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        base: u64,
        surf: &SurfaceLayout,
        rec: &TextureRecord,
        res: &Res,
    ) -> Result<(), Skip> {
        let d = res.desc;
        let info = format_info(rec.desc.format).map_err(|_| Skip::Format)?;
        if d.kind == TextureKind::D3 {
            return Err(Skip::Transfer);
        }
        // Compressed mip chains keep 1x1 blocks after reaching the block size. A mipmapped staging texture in
        // block units cannot represent those tail levels, and rounding before shifting also loses NPOT blocks.
        // Each level therefore gets a buffer with the exact block extent of that level.
        for level in 0..d.mip_levels {
            let [w, h, _] = d.level_extent(level);
            let size = [
                w.div_ceil(u32::from(info.block_w)),
                h.div_ceil(u32::from(info.block_h)),
                1,
            ];
            let staging = Arc::new(
                DataBuffer::new(
                    &self.ctx,
                    u64::from(size[0]) * u64::from(size[1]) * u64::from(surf.bytes_per_block),
                )
                .map_err(|_| Skip::Transfer)?,
            );
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
            for slice in 0..d.slice_count() {
                if slice != 0 {
                    pass.encode(&ComputeCommand::Depend(Dependency::within_pass(
                        Use::CopyRead,
                        Use::ComputeWrite,
                    )))
                    .map_err(|_| Skip::Transfer)?;
                }
                self.deswizzler
                    .encode_to_buffer(
                        pass,
                        scratch,
                        guest,
                        base + u64::from(slice) * rec.layout.layer_size + offset,
                        &staging,
                        &params,
                    )
                    .map_err(|_| Skip::Transfer)?;
                pass.encode(&ComputeCommand::Depend(Dependency::within_pass(
                    Use::ComputeWrite,
                    Use::CopyRead,
                )))
                .map_err(|_| Skip::Transfer)?;
                pass.encode(&ComputeCommand::CopyBufferToTexture {
                    buffer: staging.clone(),
                    offset: 0,
                    bytes_per_row: size[0] * surf.bytes_per_block,
                    texture: res.tex.clone(),
                    region: TextureRegion::whole(level, slice, [w, h, 1]),
                })
                .map_err(|_| Skip::Transfer)?;
            }
            pass.retain(staging);
        }
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
        pass.end();
        r
    }

    /// Encodes the copy-back of the parts of `items` that the frame wrote into guest memory, all in one compute pass
    /// that waits for the earlier work of the queue (the passes of this frame and of the frames before). A
    /// block-linear texture goes through the swizzle kernel, a pitch-linear one is a plain copy of its rows. The
    /// result of each item is the number of texel bytes it copied, or why it was refused. A refused item may have
    /// written part of its storage.
    pub(crate) fn copy_back_many(
        &self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        items: &[(&TextureRecord, &Res, &Dirty)],
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
            .map(|(rec, res, dirty)| self.copy_back_one(&mut pass, scratch, guest, rec, res, dirty))
            .collect();
        pass.end();
        out
    }

    fn copy_back_one(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        rec: &TextureRecord,
        res: &Res,
        dirty: &Dirty,
    ) -> Result<u64, Skip> {
        let d = res.desc;
        if res.widened || res.scaled || d.format.is_compressed() || d.format.is_depth() || d.format.has_stencil() {
            return Err(Skip::Transfer);
        }
        let base = guest_offset(rec, guest)?;
        let bpp = u64::from(d.format.block_bytes());
        let bytes = |level: u32| {
            let [w, h, z] = extent(&d, level);
            u64::from(w) * u64::from(h) * u64::from(z) * bpp
        };
        if rec.layout.linear {
            // NVN linear textures have one level and one layer (see `upload_linear`).
            if d.kind == TextureKind::D3 || d.mip_levels != 1 || d.array_layers != 1 || rec.desc.stride == 0 {
                return Err(Skip::Transfer);
            }
            pass.encode(&ComputeCommand::CopyTextureRegionToGuest {
                texture: res.tex.clone(),
                region: TextureRegion::whole(0, 0, extent(&d, 0)),
                guest: guest.clone(),
                offset: base,
                bytes_per_row: rec.desc.stride,
                bytes_per_image: 0,
            })
            .map_err(|_| Skip::Transfer)?;
            return Ok(bytes(0));
        }
        let surf = surface(rec, res, self.order)?;
        let slices = if d.kind == TextureKind::D3 { 1 } else { d.slice_count() };
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

/// The levels and slices of a texture that a frame wrote, for its copy-back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dirty {
    /// Every level and slice (a dispatch, a copy, a 3D attachment).
    All,
    /// These (level, slice) pairs: the attachment views of the render passes.
    Parts(Vec<(u32, u32)>),
}

impl Default for Dirty {
    fn default() -> Self {
        Self::Parts(Vec::new())
    }
}

impl Dirty {
    /// Adds one level and slice.
    pub(crate) fn add(&mut self, level: u32, slice: u32) {
        if let Self::Parts(v) = self
            && !v.contains(&(level, slice))
        {
            v.push((level, slice));
        }
    }

    /// True when nothing was written.
    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, Self::Parts(v) if v.is_empty())
    }

    /// The (level, slice) pairs inside a texture of `levels` levels and `slices` slices, in order.
    pub(crate) fn parts(&self, levels: u32, slices: u32) -> Vec<(u32, u32)> {
        match self {
            Self::All => (0..slices).flat_map(|s| (0..levels).map(move |l| (l, s))).collect(),
            Self::Parts(v) => {
                let mut v: Vec<_> = v.iter().copied().filter(|&(l, s)| l < levels && s < slices).collect();
                v.sort_unstable_by_key(|&(l, s)| (s, l));
                v
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_metal::{ArchiveConfig, ArgumentTable, DataBuffer, FrameRing, ShaderLibrary};
    use nxkit_nvn::{resource::ResourceTable, space::GpuAddr};

    /// Uploads `desc` from a pattern in guest memory, clears the memory, copies `dirty` back and returns the
    /// pattern, the guest bytes after the copy-back and the record.
    fn round_trip(desc: nxkit_nvn::layout::TextureDesc, dirty: &Dirty) -> Option<(Vec<u8>, Vec<u8>, TextureRecord)> {
        let ctx = match Context::system_default() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipped: no usable Metal 4 device ({e})");
                return None;
            }
        };
        let guest = Arc::new(GuestMemory::allocate(&ctx, 1024 * 1024).unwrap());
        let mut resources = ResourceTable::new();
        let id = resources
            .create(desc, GpuAddr::new(guest.address_map().gpu_base() + 0x1000).unwrap())
            .unwrap();
        let rec = *resources.get(id).unwrap();
        let size = rec.layout.size as usize;
        let pattern: Vec<u8> = (0..size).map(|i| (i * 7 + i / 251) as u8 | 1).collect();
        guest.write(0x1000, &pattern).unwrap();
        let mut store = crate::textures::TextureStore::new(&ctx);
        let res = store
            .ensure(&rec, 1.0, crate::textures::Use::default(), &mut Default::default())
            .unwrap();
        let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
        let transfer = Transfer::new(&ctx, &cache, GobOrder::Tegra).unwrap();
        let mut ring = FrameRing::new(&ctx).unwrap();
        let mut frame = ring.begin().unwrap();
        let scratch = frame.scratch();
        transfer
            .upload(frame.commands(), &scratch, &guest, &rec, &res, false)
            .unwrap();
        ctx.commit_residency();
        frame.submit().unwrap().wait().unwrap();
        guest.write(0x1000, &vec![0; size]).unwrap();
        let mut frame = ring.begin().unwrap();
        let scratch = frame.scratch();
        let results = transfer.copy_back_many(frame.commands(), &scratch, &guest, &[(&rec, &res, dirty)]);
        assert!(results[0].is_ok(), "{results:?}");
        ctx.commit_residency();
        frame.submit().unwrap().wait().unwrap();
        let mut out = vec![0; size];
        guest.read(0x1000, &mut out).unwrap();
        Some((pattern, out, rec))
    }

    #[test]
    fn copy_back_writes_the_dirty_levels_and_layers_of_a_block_linear_array() {
        let desc = nxkit_nvn::layout::TextureDesc {
            target: Target::D2Array,
            format: 0x06,
            flags: 0,
            samples: 0,
            width: 64,
            height: 64,
            depth: 3,
            levels: 2,
            stride: 0,
            packaged_block: None,
        };
        let mut dirty = Dirty::default();
        dirty.add(0, 0);
        dirty.add(1, 2);
        let Some((pattern, out, rec)) = round_trip(desc, &dirty) else { return };
        // Both levels fill whole GOB blocks, so every byte of a level is a texel.
        let level = |l: u32| {
            let lo = desc.level_offset(l).unwrap();
            let len = (64u64 >> l) * (64u64 >> l) * 2;
            (lo, len)
        };
        for slice in 0..3u64 {
            for l in 0..2 {
                let (lo, len) = level(l);
                let at = (slice * rec.layout.layer_size + lo) as usize;
                let got = &out[at..at + len as usize];
                if dirty.parts(2, 3).contains(&(l, slice as u32)) {
                    assert_eq!(got, &pattern[at..at + len as usize], "level {l} slice {slice}");
                } else {
                    assert!(got.iter().all(|b| *b == 0), "level {l} slice {slice} was written");
                }
            }
        }
    }

    #[test]
    fn copy_back_writes_the_rows_of_a_pitch_linear_texture() {
        let desc = nxkit_nvn::layout::TextureDesc {
            target: Target::D2,
            format: 0x25,
            flags: nxkit_nvn::layout::FLAG_LINEAR_RENDER_TARGET,
            samples: 0,
            width: 20,
            height: 8,
            depth: 1,
            levels: 1,
            stride: 128,
            packaged_block: None,
        };
        let Some((pattern, out, _)) = round_trip(desc, &Dirty::All) else { return };
        for row in 0..8 {
            let at = row * 128;
            assert_eq!(&out[at..at + 80], &pattern[at..at + 80], "row {row}");
            assert!(out[at + 80..at + 128].iter().all(|b| *b == 0), "the padding of row {row} was written");
        }
    }

    #[test]
    fn compressed_upload_preserves_every_mip_and_array_layer() {
        let ctx = match Context::system_default() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipped: no usable Metal 4 device ({e})");
                return;
            }
        };
        let guest = Arc::new(GuestMemory::allocate(&ctx, 1024 * 1024).unwrap());
        let desc = nxkit_nvn::layout::TextureDesc {
            target: Target::D2Array,
            format: 0x41,
            flags: 0,
            samples: 0,
            width: 20,
            height: 12,
            depth: 2,
            levels: 5,
            stride: 0,
            packaged_block: None,
        };
        let mut resources = ResourceTable::new();
        let id = resources
            .create(desc, GpuAddr::new(guest.address_map().gpu_base()).unwrap())
            .unwrap();
        let rec = resources.get(id).unwrap();
        // Every block in a level has a constant BC1 colour, with a distinct colour in each level and layer.
        for slice in 0u32..2 {
            for level in 0..5 {
                let color = if slice == 0 {
                    (31u16 - level as u16) << 11
                } else {
                    (63u16 - level as u16) << 5
                };
                let block = [color as u8, (color >> 8) as u8, 0, 0, 0, 0, 0, 0];
                let lo = desc.level_offset(level).unwrap();
                let hi = desc.level_offset(level + 1).unwrap();
                let bytes: Vec<u8> = block.into_iter().cycle().take((hi - lo) as usize).collect();
                guest
                    .write(u64::from(slice) * rec.layout.layer_size + lo, &bytes)
                    .unwrap();
            }
        }
        let mut store = crate::textures::TextureStore::new(&ctx);
        let res = store
            .ensure(
                rec,
                1.0,
                crate::textures::Use::default(),
                &mut Default::default(),
            )
            .unwrap();
        let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
        let transfer = Transfer::new(&ctx, &cache, GobOrder::Tegra).unwrap();
        let output = Arc::new(DataBuffer::new(&ctx, 10 * 16).unwrap());
        let lib = ShaderLibrary::from_source(&ctx, "sample_mips", r#"
            #include <metal_stdlib>
            using namespace metal;
            kernel void sample_mips(device float4* dst [[buffer(0)]], texture2d_array<float> tex [[texture(0)]],
                uint2 gid [[thread_position_in_grid]]) {
                constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::nearest, mip_filter::nearest);
                dst[gid.y * 5 + gid.x] = tex.sample(s, float2(0.5), gid.y, level(float(gid.x)));
            }
        "#).unwrap();
        let pipeline = cache.compute(123, &lib, "sample_mips").unwrap();
        let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
        let mut ring = FrameRing::new(&ctx).unwrap();
        let mut frame = ring.begin().unwrap();
        let scratch = frame.scratch();
        transfer
            .upload(frame.commands(), &scratch, &guest, rec, &res, false)
            .unwrap();
        {
            let mut cp = frame.commands().begin_compute_pass(&table).unwrap();
            cp.encode(&ComputeCommand::Depend(Dependency::between(
                Use::CopyWrite,
                Use::ComputeRead,
            )))
            .unwrap();
            cp.encode(&ComputeCommand::SetPipeline(pipeline)).unwrap();
            cp.encode(&ComputeCommand::SetBuffer {
                index: 0,
                gpu_addr: output.gpu_address(),
            })
            .unwrap();
            cp.encode(&ComputeCommand::SetTexture {
                index: 0,
                texture: res.tex.clone(),
            })
            .unwrap();
            cp.encode(&ComputeCommand::DispatchThreads {
                grid: [5, 2, 1],
                threads: [5, 2, 1],
            })
            .unwrap();
            cp.retain(output.clone());
            cp.end();
        }
        ctx.commit_residency();
        frame.submit().unwrap().wait().unwrap();
        let mut bytes = [0; 160];
        output.read(0, &mut bytes).unwrap();
        for slice in 0..2 {
            for level in 0..5 {
                let at = (slice * 5 + level) * 16;
                let rgba: Vec<f32> = bytes[at..at + 16]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect();
                let expected = if slice == 0 {
                    (31 - level) as f32 / 31.0
                } else {
                    (63 - level) as f32 / 63.0
                };
                assert!(
                    (rgba[slice] - expected).abs() < 0.005,
                    "slice {slice} mip {level}: {rgba:?}, expected {expected}"
                );
                assert_eq!(rgba[1 - slice], 0.0);
                assert_eq!(rgba[3], 1.0);
            }
        }
    }
}
