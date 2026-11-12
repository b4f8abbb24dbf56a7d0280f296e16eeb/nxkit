//! Steps of the frame encode that stand apart: the rename slot, the write tracking, the uploads and the private mode passes.

use super::*;

/// What `Gpu::track_writes` finds: the first pass that writes each texture, the textures that the GPU wrote, and the
/// levels and layers that were written with the last pass to write them.
type Written = (
    FastMap<TexId, usize>,
    Vec<TexId>,
    FastMap<TexId, (Dirty, usize)>,
);

impl Gpu {
    /// The storage ranges that the frame reads from a copy of its own (the overlap, `rename`): the map where
    /// the bindings read, the buffer of the slot and the merge ranges.
    pub(super) fn rename_begin(
        &self,
        detail: &FrameDetail,
        pools: &Arc<GuestPools>,
        region: usize,
    ) -> (
        crate::rename::RenameMap,
        Option<Arc<DataBuffer>>,
        Vec<crate::rename::MergeRun>,
    ) {
        if crate::overlap::on() {
            self.rename.note_rw(detail, &self.programs);
            let b = self
                .rename
                .begin_slot(region, detail.rename.as_ref(), self.gmem.learned());
            // The merge ranges: where the guest range really is. The copy of each is filled and merged back on the GPU.
            let runs: Vec<crate::rename::MergeRun> = b
                .merges
                .into_iter()
                .filter_map(|(cap, idx, addr, size, slot, copy2)| {
                    let guest = pools.resolve(addr, u64::from(size)).ok()?.real;
                    Some(crate::rename::MergeRun {
                        cap,
                        idx,
                        guest,
                        slot,
                        copy2,
                    })
                })
                .collect();
            (b.map, b.buf, runs)
        } else {
            (crate::rename::RenameMap::new(), None, Vec::new())
        }
    }

    /// A render target that is copied back stays at the native size, and the other attachments of its passes with it.
    pub(super) fn pin_copyback_targets(
        &self,
        plan: &PassPlan,
        view: &dyn EncodeView,
        uses: &mut FastMap<TexId, Use>,
    ) {
        if crate::copyback::enabled() {
            let mut pin: FastSet<TexId> = uses
                .iter()
                .filter(|(id, u)| {
                    u.attachment
                        && view
                            .record(**id)
                            .is_ok_and(|rec| self.copyback.candidate(**id, rec).is_some())
                })
                .map(|(id, _)| *id)
                .collect();
            if !pin.is_empty() {
                let mut co = Vec::new();
                for p in plan.passes.iter().filter(|p| p.kind == PassKind::Render) {
                    let atts = plan.attachments_of(p);
                    if atts.iter().any(|a| pin.contains(&a.tex)) {
                        co.extend(atts.iter().map(|a| a.tex).filter(|t| !pin.contains(t)));
                    }
                }
                for t in co {
                    static SHOWN: std::sync::atomic::AtomicU32 =
                        std::sync::atomic::AtomicU32::new(0);
                    if pin.insert(t)
                        && crate::diag("copyback")
                        && SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 50
                    {
                        eprintln!(
                            "DIAG copyback pins {t:?} at the native size, it shares a pass with a copied-back texture"
                        );
                    }
                }
                for id in pin {
                    if let Some(u) = uses.get_mut(&id) {
                        u.cpu_read = true;
                    }
                }
            }
        }
    }

    /// The first pass that writes each texture, the textures that the GPU wrote and the levels and layers that
    /// the frame wrote (for the copy-back).
    pub(super) fn track_writes(
        frame: &Frame,
        plan: &PassPlan,
        textures: &FastMap<TexId, Res>,
        pass_images: &[crate::images::PassImages],
    ) -> Written {
        let mut first_write: FastMap<TexId, usize> = FastMap::default();
        let mut gpu_wrote: Vec<TexId> = Vec::new();
        // The levels and layers that the frame wrote, and the last pass that wrote them, for the copy-back.
        let mut dirty: FastMap<TexId, (Dirty, usize)> = FastMap::default();
        for (i, p) in plan.passes.iter().enumerate() {
            if !matches!(
                p.kind,
                PassKind::Render | PassKind::Compute | PassKind::Copy
            ) {
                continue;
            }
            let extra = pass_images.get(i).map_or(&[][..], |x| x.writes.as_slice());
            for id in touch(frame, plan, p)
                .writes
                .into_iter()
                .chain(extra.iter().copied())
            {
                let memoryless = textures
                    .get(&id)
                    .is_none_or(|r| r.desc.storage == nxkit_metal::Storage::Memoryless);
                if !memoryless && first_write.insert(id, i).is_none() {
                    gpu_wrote.push(id);
                }
                if !memoryless && crate::copyback::enabled() {
                    let e = dirty.entry(id).or_default();
                    e.1 = i;
                    if p.kind != PassKind::Render {
                        e.0 = Dirty::All;
                    }
                }
            }
            if p.kind == PassKind::Render {
                for a in plan.attachments_of(p) {
                    let Some((d, _)) = dirty.get_mut(&a.tex) else {
                        continue;
                    };
                    let three_d = textures
                        .get(&a.tex)
                        .is_some_and(|r| r.desc.kind == nxkit_metal::TextureKind::D3);
                    if three_d {
                        *d = Dirty::All;
                    } else if a.store == nxkit_nvn::passes::StoreAction::Store {
                        d.add(a.view.level(), a.view.layer());
                    }
                }
            }
        }
        (first_write, gpu_wrote, dirty)
    }

    /// Registers the early cuts of the frame at the batch: the pages that a cut releases when its prefix is complete.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn register_early_cuts(
        &self,
        early: &mut crate::private::EarlyPlan,
        ends: &[usize],
        batch: &mut nxkit_metal::Batch,
        fence: u64,
        dirty: &FastMap<TexId, (Dirty, usize)>,
        frame: &Frame,
        view: &dyn EncodeView,
    ) {
        let mut excluded: Vec<(u64, u64)> = Vec::new();
        let ids = dirty
            .keys()
            .copied()
            .chain(frame.cmds.iter().filter_map(|c| match c {
                Cmd::CpuRead(id) => Some(*id),
                _ => None,
            }));
        for id in ids {
            if let Ok(rec) = view.record(id) {
                excluded.push((rec.address.get(), rec.end()));
            }
        }
        crate::private::early_exclude(&mut early.cuts, &excluded, &self.guest.address_map());
        let mut registered = Vec::new();
        for cut in std::mem::take(&mut early.cuts) {
            let end = cut.after_pass + 1;
            let count = ends.iter().position(|e| *e == end).map_or(0, |i| i + 2);
            if batch.on_prefix_complete(
                count,
                crate::private::early_release(fence, cut.pages.clone()),
            ) {
                registered.push(cut);
            }
        }
        early.cuts = registered;
    }

    /// `NXKIT_PRIVATE=1`: the pages that the CPU wrote since the frame before go into the mirror.
    pub(super) fn copy_in_pages(
        &self,
        pools: &Arc<GuestPools>,
        first: &mut CommandBuffer,
        scratch: &Scratch,
        stats: &mut EncodeStats,
    ) {
        if let Some(o) = pools.overlay() {
            let (jobs, lost) = crate::private::copy_in_jobs(o);
            if !jobs.is_empty() || lost > 0 {
                let done = nxkit_metal::ArgumentTable::new(&self.ctx, 31, 0)
                    .ok()
                    .and_then(|t| {
                        crate::private::encode_copy_in(&self.pipelines, first, &t, scratch, &jobs)
                            .ok()
                    });
                match done {
                    Some(n) => crate::private::note_copy_in(n, lost),
                    None => {
                        stats.skip(Skip::Encode);
                        crate::private::note_copy_in(0, lost + jobs.len() as u32);
                    }
                }
            }
        }
    }

    /// Results one frame late: the copy of the late pages at the start of the frame, with the jobs for its end.
    pub(super) fn late_copies_start(
        &self,
        pools: &Arc<GuestPools>,
        first: &mut CommandBuffer,
        scratch: &Scratch,
        stats: &mut EncodeStats,
    ) -> Option<(
        Vec<crate::private::CopyJob>,
        Vec<crate::private::CopyJob>,
        u64,
        u64,
    )> {
        let mut late_copies = pools
            .overlay()
            .and_then(|o| crate::private::late_copy_jobs(o));
        if let Some((start, ..)) = &late_copies {
            let ok = nxkit_metal::ArgumentTable::new(&self.ctx, 31, 0)
                .ok()
                .and_then(|t| {
                    crate::private::encode_copy_in(&self.pipelines, first, &t, scratch, start).ok()
                });
            if ok.is_none() {
                stats.skip(Skip::Encode);
                late_copies = None;
            }
        }
        late_copies
    }

    /// Uploads the textures that the guest changed, scaled or plain, into the first command buffer. Returns the
    /// textures that were deswizzled.
    pub(super) fn upload_textures(
        &mut self,
        view: &dyn EncodeView,
        first: &mut CommandBuffer,
        scratch: &Scratch,
        pools: &Arc<GuestPools>,
        textures: &FastMap<TexId, Res>,
        stats: &mut EncodeStats,
    ) -> Vec<TexId> {
        let mut deswizzled = Vec::new();
        self.transfer.begin_frame();
        let mut zeroer = freshzero::Zeroer::new(crate::knob("freshzero"));
        let mut scaled_queued = Vec::new();
        for (id, res) in textures {
            let Ok(rec) = view.record(*id) else { continue };
            if TextureStore::needs_scaled_upload(res, rec) {
                // The guest layout has the native size: a staging texture takes it and a kernel scales it.
                let wait = self.copyback.upload_waits(rec);
                let t_up = Instant::now();
                let r = self
                    .transfer
                    .upload_scaled(first, scratch, pools, rec, res, wait);
                stats.upload_ns += nanos(t_up, Instant::now());
                match r {
                    Ok(()) => scaled_queued.push((*id, res.barrier_id)),
                    Err(why) => {
                        stats.scaled_upload_lost += 1;
                        stats.skip(why);
                    }
                }
                continue;
            }
            if crate::diag("stale")
                && crate::diag::tex_match(
                    res.desc.format,
                    res.desc.width,
                    res.desc.height,
                    res.desc.array_layers,
                )
            {
                self.stale_diag(*id, rec, res);
            }
            if res.in_place.is_some() {
                // The texture is the guest bytes: a CPU write needs no upload, and the record is in sync.
                if rec.residency == nxkit_nvn::resource::Residency::GuestNewer {
                    deswizzled.push(*id);
                }
                continue;
            }
            if !TextureStore::needs_upload(res, rec) {
                if zeroer.zero(&self.ctx, first, res) {
                    self.barriers.seed(res.barrier_id, Stages::FRAGMENT);
                }
                continue;
            }
            let wait = self.copyback.upload_waits(rec);
            let t_up = Instant::now();
            let r = self.transfer.upload(first, scratch, pools, rec, res, wait);
            stats.upload_ns += nanos(t_up, Instant::now());
            static UPLOADS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if crate::diag("upload")
                && crate::diag::tex_filtered()
                && crate::diag::tex_match(
                    res.desc.format,
                    res.desc.width,
                    res.desc.height,
                    res.desc.array_layers,
                )
                && crate::diag::first(&UPLOADS, 200)
            {
                eprintln!(
                    "DIAG upload {id:?} {:?} {}x{} residency {:?} scaled {} linear {}: {:?}",
                    res.desc.format,
                    res.desc.width,
                    res.desc.height,
                    rec.residency,
                    res.scaled,
                    rec.layout.linear,
                    r.as_ref().err()
                );
            }
            match r {
                Ok(()) => {
                    stats.uploads += 1;
                    stats.compute_passes += 1;
                    deswizzled.push(*id);
                    self.barriers
                        .seed(res.barrier_id, Stages::DISPATCH | Stages::BLIT);
                }
                Err(why) => stats.skip(why),
            }
        }

        zeroer.report();
        if !scaled_queued.is_empty() {
            let (scaled, lost) = self.transfer.flush_scaled(first);
            stats.scaled_uploads += scaled;
            stats.uploads += scaled;
            stats.scaled_upload_lost += lost;
            stats.compute_passes += 1;
            // The scaling writes the texture with compute stages, like the plain upload. A texture that lost part of
            // its write still changed, so every queued texture gets the seed.
            for (id, barrier_id) in scaled_queued {
                if lost == 0 {
                    deswizzled.push(id);
                }
                self.barriers
                    .seed(barrier_id, Stages::DISPATCH | Stages::BLIT);
            }
        }
        deswizzled
    }

    /// The end of the frame in the private mode: the late stage pages and the late copies go in again.
    pub(super) fn late_stage_end(
        &self,
        pools: &Arc<GuestPools>,
        first: &mut CommandBuffer,
        rest: &mut [CommandBuffer],
        scratch: &Scratch,
        late_copies: &Option<(
            Vec<crate::private::CopyJob>,
            Vec<crate::private::CopyJob>,
            u64,
            u64,
        )>,
        stats: &mut EncodeStats,
    ) {
        if let Some(o) = pools.overlay() {
            let jobs = crate::private::late_stage(o);
            if !jobs.is_empty() {
                let ok = nxkit_metal::ArgumentTable::new(&self.ctx, 31, 0)
                    .ok()
                    .and_then(|t| {
                        crate::private::encode_copy_in(&self.pipelines, first, &t, scratch, &jobs)
                            .ok()
                    });
                if ok.is_none() {
                    stats.skip(Skip::Encode);
                }
            }
        }
        if let (Some((_, end, start_at, end_at)), Some(o)) = (&late_copies, pools.overlay()) {
            let cb = match rest.last_mut() {
                Some(cb) => cb,
                None => &mut *first,
            };
            let ok = nxkit_metal::ArgumentTable::new(&self.ctx, 31, 0)
                .ok()
                .and_then(|t| {
                    crate::private::encode_copy_in(&self.pipelines, cb, &t, scratch, end).ok()
                });
            match ok {
                Some(_) => crate::private::late_copies_encoded(o.frame(), *start_at, *end_at),
                None => stats.skip(Skip::Encode),
            }
        }
    }
}
