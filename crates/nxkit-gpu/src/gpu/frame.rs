//! The encode of a frame: the passes cut into chunks, run on the threads and committed in order.

use super::*;

/// The weight of each pass of the plan for the cut into chunks: its commands and one.
fn weights_of(plan: &PassPlan) -> Vec<u32> {
    plan.passes
        .iter()
        .map(|p| p.cmds.len() as u32 + 1)
        .collect()
}

/// Whether the encode streams its command buffers (`NXKIT_STREAM_COMMIT`, on unless `0`): the first command buffers of
/// a frame are committed as soon as they are encoded, while the others are encoded.
fn stream_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !matches!(std::env::var("NXKIT_STREAM_COMMIT").as_deref(), Ok("0")))
}

/// The share of the weight of a frame that its lead chunk takes at least: one in this many.
const LEAD_SHARE: u64 = 8;

/// The frames with fewer passes than this are not streamed: their encode is short.
const STREAM_MIN_PASSES: usize = 8;

/// Puts a lead chunk in front of `ends` (the end of each chunk): the first passes up to about one in [`LEAD_SHARE`] of
/// the weight. Returns true when the lead was added, which needs at least [`STREAM_MIN_PASSES`] passes, a first chunk
/// that is larger than the lead, one chunk more after it (the last command buffer is never in the streamed group), and
/// room for `1 + ends.len() + 1` command buffers within `max_buffers`.
fn lead_chunk(weights: &[u32], ends: &mut Vec<usize>, max_buffers: usize) -> bool {
    if weights.len() < STREAM_MIN_PASSES || ends.len() < 2 || 2 + ends.len() > max_buffers {
        return false;
    }
    let total: u64 = weights.iter().map(|w| u64::from(*w)).sum();
    let want = total.div_ceil(LEAD_SHARE).max(1);
    let (mut acc, mut lead) = (0u64, 0usize);
    for (i, w) in weights.iter().enumerate() {
        acc += u64::from(*w);
        lead = i + 1;
        if acc >= want {
            break;
        }
    }
    if lead >= ends[0] {
        return false;
    }
    ends.insert(0, lead);
    true
}

pub(super) fn run_chunk(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    keep: &Arc<Keep>,
    range: Range<usize>,
) -> (EncodeStats, Vec<TexId>) {
    nxkit_metal::autorelease_pool(|| run_chunk_inner(sh, cb, scratch, keep, range))
}

pub(super) fn run_chunk_inner(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    keep: &Arc<Keep>,
    range: Range<usize>,
) -> (EncodeStats, Vec<TexId>) {
    let mut stats = EncodeStats::default();
    let mut copied = Vec::new();
    cb.retain(keep.clone());
    let Ok(tables) = Tables::new(sh.ctx) else {
        stats.skipped[Skip::Encode as usize] += 1;
        return (stats, copied);
    };
    if let Some(p) = sh.prof {
        p.chunk_start(range.start);
    }
    for index in range {
        let Some(pass) = sh.plan.passes.get(index) else {
            continue;
        };
        match pass.kind {
            PassKind::Render => render_pass(sh, cb, &tables, scratch, index, pass, &mut stats),
            PassKind::Compute | PassKind::Copy => {
                compute_pass(sh, cb, &tables, scratch, index, pass, &mut stats)
            }
            PassKind::CpuRead => copied.extend(cpu_read(sh, cb, scratch, index, pass, &mut stats)),
            PassKind::Sync | PassKind::Present => {}
        }
    }
    (stats, copied)
}

impl Gpu {
    /// Encodes a held frame and submits it.
    ///
    /// `frame` and `plan` come from `nxkit-nvn` (`Frame` and `analyse`). `detail` carries what the commands of the
    /// frame bound, `view` gives the texture records and resolves texture handles. `pools` is the pool snapshot of
    /// the flush; the batch keeps it until the GPU is done. `scale` is the factor for textures that are rendered
    /// into (2.0 for 1080p to 4K, 1.0 for none). The call returns when the command buffers are committed. The GPU
    /// runs on. Call [`Gpu::wait`] before the CPU reads a texture that the report lists in `copied_back`.
    pub fn encode_frame(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        view: &dyn EncodeView,
        pools: &Arc<GuestPools>,
        detail: &FrameDetail,
        scale: f32,
    ) -> Result<EncodeReport> {
        nxkit_metal::autorelease_pool(|| {
            self.encode_frame_inner(frame, plan, view, pools, detail, scale)
        })
    }

    fn encode_frame_inner(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        view: &dyn EncodeView,
        pools: &Arc<GuestPools>,
        detail: &FrameDetail,
        scale: f32,
    ) -> Result<EncodeReport> {
        crate::refresh_knobs();
        self.native.begin_frame();
        let diag_frame = crate::diag::begin_frame();
        let lum = crate::lum::begin();
        let scan = crate::nanscan::begin(&self.ctx);
        let t0 = Instant::now();
        MARKS.with(|m| m.borrow_mut().clear());
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let mut stats = EncodeStats::default();
        let builds_before = self.pipelines.counts().0;

        // The batch comes first, so that the uniform snapshots go to its scratch memory on a helper thread while this
        // one resolves the textures; the chunks need both.
        let mut ends = if plan.passes.is_empty() {
            Vec::new()
        } else {
            jobs::split(&weights_of(plan), self.threads * CHUNKS_PER_THREAD)
        };
        // `NXKIT_PRIVATE=1`: the pages that game threads waited for lately as the results of this GPU are signalled when the
        // last pass that writes them is done. The frame is cut after those passes: each cut ends a command buffer, and the
        // part before it is committed on its own.
        let fence = pools.overlay().map_or(0, |o| o.frame());
        let mut early = if fence != 0 && !plan.passes.is_empty() && crate::private::early_enabled() {
            crate::private::early_cuts(
                frame,
                plan,
                detail,
                fence,
                &self.guest.address_map(),
                self.gmem.learned(),
            )
        } else {
            crate::private::EarlyPlan::default()
        };
        for cut in &early.cuts {
            if let Err(at) = ends.binary_search(&(cut.after_pass + 1)) {
                ends.insert(at, cut.after_pass + 1);
            }
        }
        // The first part of the frame is committed while the rest is encoded (`stream_commit`): a small lead chunk comes
        // first, so that the group of the first command buffer (uploads) and the lead chunk is ready early. A frame with
        // early cuts has its own prefix.
        let max_buffers = self.threads * CHUNKS_PER_THREAD + 1 + crate::private::EARLY_CUTS;
        let stream = early.cuts.is_empty()
            && stream_enabled()
            && !crate::private::mirrored()
            && lead_chunk(&weights_of(plan), &mut ends, max_buffers);
        let t_ring = Instant::now();
        let mut batch = self.ring.begin(1 + ends.len())?;
        PHASES.with(|p| p.borrow_mut()[0] = nanos(t_ring, Instant::now()));
        // The ring waited for the batch that used this slot before, so its write logs are final.
        let region = ((batch.number() - 1) % nxkit_metal::BATCHES_IN_FLIGHT as u64) as usize;
        self.gmem.learn(region);
        // `NXKIT_PRIVATE=track`: what the programs write and which pools shaders wrote.
        crate::private::note_rw(detail, &self.programs);
        crate::private::note_learned(self.gmem.learned());
        // The storage ranges that this frame reads from a copy of its own (the overlap, `rename`). The slot's earlier
        // frame is written back first.
        let (rename_map, rename_buf, merges) = if crate::overlap::on() {
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
        };
        let merge_table = if merges.is_empty() {
            None
        } else {
            nxkit_metal::ArgumentTable::new(&self.ctx, 31, 0).ok()
        };
        let gmem_buf = self.gmem.refresh(&self.ctx, pools);
        self.gmem.logs().begin(region, self.gmem.ranges());
        let gmem_logs = self.gmem.logs().clone();
        let scratch = batch.scratch();
        let preload = preload::start(detail, &scratch);
        mark(t0, "ring");
        let mut uses = collect_uses(frame, plan);
        // The textures the fragment shaders of draws bind as images are not in the frame's lists. They are used (not
        // memoryless), and a texture with image atomics is made for them.
        let image_uses = crate::images::uses(&mut self.img_decl, &self.programs, view, detail);
        for id in &image_uses.all {
            let u = uses.entry(*id).or_default();
            u.memoryless = false;
            u.atomic |= image_uses.atomic.contains(id);
        }
        self.copyback.note_presented(frame);
        // A render target that is copied back stays at the native size: the copy-back needs the guest layout. The
        // other attachments of its passes stay at the native size too, since a pass renders at one scale.
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
        for (id, u) in &uses {
            self.store.note(*id, *u);
        }
        self.store.set_pools(pools);
        let mut textures: FastMap<TexId, Res> = FastMap::default();
        for (id, u) in &uses {
            match view.record(*id) {
                Ok(rec) => match self.store.ensure(rec, scale, *u, &mut stats) {
                    Ok(r) => {
                        textures.insert(*id, r);
                    }
                    Err(why) => stats.skip(why),
                },
                Err(e) => stats.skip(crate::tex_skip(format_args!("use {id:?}: {e:?}"))),
            }
        }
        mark(t0, "uses+ensure");
        let t_resolve = Instant::now();
        let pool_ids = self.pool_prepare(view, detail, &mut stats);
        let mut handles =
            self.resolve_handles(view, detail, &pool_ids, scale, &mut textures, &mut stats);
        let t_fit = Instant::now();
        self.fit_handles(view, detail, &textures, &mut handles, &mut stats);
        crate::images::resolve(
            &mut self.store,
            &mut self.img_decl,
            &self.programs,
            view,
            detail,
            scale,
            &mut textures,
            &mut handles,
            &mut stats,
        );
        stats.resolve_ns = nanos(t_resolve, t_fit);
        stats.fit_ns = nanos(t_fit, Instant::now());
        // Textures that were idle for long and whose guest copy is current go here, with the replaced ones.
        self.store.end_frame();
        drop(self.store.take_retired());
        self.store.trim_views();
        // The store dropped or replaced Metal textures above: no element may keep the resource ID of one of them.
        self.pool_release();

        mark(t0, "resolve+fit");
        self.imgdump_step(view, detail, &textures);
        let ords = ordinals(frame);
        let native = self.native.prepare(
            &crate::native::PrepareCx {
                ctx: &self.ctx,
                guest: &self.guest,
                pipelines: &self.pipelines,
                frame,
                detail,
                view,
                textures: &textures,
                store: &self.store,
                pools: (&self.copyback, self.gmem.learned()),
            },
            &mut stats,
        );
        let mut pass_images = crate::images::pass_images(frame, plan, &ords, &image_uses.per_draw);
        if let Some(p) = &native {
            crate::native::book_writes(p, frame, plan, &ords, &mut pass_images);
        }
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

        for id in &gpu_wrote {
            self.store.mark_written(*id);
        }
        pass_shape_diag(frame, plan, &textures);
        mark(t0, "pre-ring");
        let keep: Arc<Keep> = {
            // Every write of the frame to the pool tables is done (`pool_prepare`, `resolve_handles`, `pool_release`,
            // the images). The ring waited for the batch that used `region` before, so no frame reads its copy.
            self.bindless.sync(region);
            let mut tables = self.bindless.pool_buffers(region);
            tables.extend(rename_buf);
            let mut all: Vec<Texture> = textures.values().map(|r| r.tex.clone()).collect();
            all.extend(handles.keep);
            Arc::new((all, tables, gmem_buf.clone(), pools.clone()))
        };

        // The pass signals: the pages of a cut that a texture copy-back or a read of the CPU writes are left out (the last command
        // buffer writes them, whichever pass the planner named).
        let mut ahead = None;
        if !early.cuts.is_empty() {
            let mut excluded: Vec<(u64, u64)> = Vec::new();
            let ids = dirty.keys().copied().chain(frame.cmds.iter().filter_map(|c| match c {
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
                if batch.on_prefix_complete(count, crate::private::early_release(fence, cut.pages.clone())) {
                    registered.push(cut);
                }
            }
            early.cuts = registered;
            // The first part can go to the GPU while the rest is encoded, unless the first command buffer still gets the pages
            // of a late stage.
            if let Some(o) = pools.overlay()
                && !early.cuts.is_empty()
                && !o.forced()
                && !crate::knob("privlatestage")
                && !crate::knob("privnoahead")
            {
                ahead = batch.commit_ahead();
            }
        }

        if stream && ahead.is_none() {
            ahead = batch.stream_prefix(2);
        }

        let mut deswizzled = Vec::new();
        let (first, rest) = batch
            .buffers()
            .split_first_mut()
            .ok_or(MetalError::Invalid("the batch has no buffers"))?;
        first.retain(keep.clone());
        // `NXKIT_PRIVATE=1`: the pages that the CPU wrote since the frame before go into the mirror before anything of this
        // frame reads or writes it (the texture uploads below may read member pages).
        if let Some(o) = pools.overlay() {
            let (jobs, lost) = crate::private::copy_in_jobs(o);
            if !jobs.is_empty() || lost > 0 {
                let done = nxkit_metal::ArgumentTable::new(&self.ctx, 31, 0)
                    .ok()
                    .and_then(|t| {
                        crate::private::encode_copy_in(&self.pipelines, first, &t, &scratch, &jobs).ok()
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
        // Results one frame late: the GPU copies the late pages as the frame starts with them (after the copy-in) and,
        // below, as it ends, so that its completion hands the readers the frame's own bytes and knows which blocks it
        // changed.
        let mut late_copies = pools.overlay().and_then(|o| crate::private::late_copy_jobs(o));
        if let Some((start, ..)) = &late_copies {
            let ok = nxkit_metal::ArgumentTable::new(&self.ctx, 31, 0)
                .ok()
                .and_then(|t| crate::private::encode_copy_in(&self.pipelines, first, &t, &scratch, start).ok());
            if ok.is_none() {
                stats.skip(Skip::Encode);
                late_copies = None;
            }
        }
        let mesh = crate::meshvis::plan(
            diag_frame,
            frame,
            plan,
            detail,
            &ords,
            &textures,
            &self.guest,
        );
        let queries =
            crate::queries::prepare(&self.ctx, &self.vis_pool, frame, plan, detail, &ords, mesh);
        if let Some(b) = queries.as_ref().and_then(|q| q.buffer()) {
            first.retain(b.clone());
        }
        self.transfer.begin_frame();
        let mut zeroer = freshzero::Zeroer::new(crate::knob("freshzero"));
        let mut scaled_queued = Vec::new();
        for (id, res) in &textures {
            let Ok(rec) = view.record(*id) else { continue };
            if TextureStore::needs_scaled_upload(res, rec) {
                // The guest layout has the native size: a staging texture takes it and a kernel scales it.
                let wait = self.copyback.upload_waits(rec);
                let t_up = Instant::now();
                let r = self
                    .transfer
                    .upload_scaled(first, &scratch, pools, rec, res, wait);
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
            let r = self.transfer.upload(first, &scratch, pools, rec, res, wait);
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

        let aliases = textures
            .iter()
            .map(|(id, res)| (*id, res.barrier_id))
            .collect();
        mark(t0, "pre-ords");
        kpass_diag(frame, plan, detail, &ords, &textures);
        mark(t0, "ords+kpass");
        let t_barrier = Instant::now();
        // Render passes write compressed attachments through block shadows and a blit (`textures::blocks`).
        let blit_written = textures
            .values()
            .filter(|r| r.desc.format.is_compressed())
            .map(|r| r.barrier_id)
            .collect();
        let vertex_samples = vertex_sampling_passes(frame, plan, detail, &ords);
        let barrier_plan = self.barriers.plan(
            frame,
            plan,
            &aliases,
            &vertex_samples,
            &pass_images,
            &blit_written,
        );
        stats.barrier_ns = nanos(t_barrier, Instant::now());
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            if crate::diag("barr")
                && N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .is_multiple_of(120)
            {
                let d = barrier_plan.distance;
                eprintln!(
                    "DIAG barr distance to the work waited for: earlier frame {} | 1 {} | 2 {} | 3-5 {} | more {} (of {} barriers)",
                    d[0], d[1], d[2], d[3], d[4], barrier_plan.emitted
                );
            }
        }
        mark(t0, "barriers");
        let run_at: FastMap<u32, RecordingRun> =
            detail.runs.iter().map(|r| (r.first_draw, *r)).collect();
        let preloaded = preload::finish(preload);
        mark(t0, "preload");
        // The merge ranges are filled from guest memory on the GPU before the first pass of the frame.
        let t_merge = crate::diag("rename").then(Instant::now);
        if let Some(table) = &merge_table
            && !ends.is_empty()
            && crate::rename::merge_encode(
                &self.pipelines,
                first,
                table,
                &scratch,
                &merges,
                crate::rename::Phase::In,
            )
            .is_err()
        {
            stats.skip(Skip::Encode);
        }
        if let Some(t) = t_merge {
            self.rename.cost_add(7, nanos(t, Instant::now()));
        }
        let uploaded = Mutex::new(FastMap::default());
        let prof = if crate::passprof::on() {
            crate::passprof::begin_frame(&self.ctx, plan, |i| {
                describe_pass(frame, plan, detail, &ords, &textures, i)
            })
        } else {
            None
        };
        let sh = Shared {
            ctx: &self.ctx,
            frame,
            plan,
            detail,
            table: view,
            textures: &textures,
            texture_store: &self.store,
            programs: &self.programs,
            pipelines: &self.pipelines,
            env: Env {
                handles: &handles.map,
                fits: &handles.fits,
                images: &handles.images,
                gmem: crate::gmem::GmemEnv {
                    map: gmem_buf.as_ref().map_or(0, |b| b.gpu_address()),
                    logs: Some((&*gmem_logs, region)),
                },
                pools: self.bindless.pools(region),
                rename: &rename_map,
                guest_pools: pools,
                default_sampler: self.bindless.default_sampler_id(),
                statics: self.bindless.statics(),
                scale,
            },
            guest: &self.guest,
            pools,
            icbs: &self.icbs,
            ordinals: &ords,
            run_at: &run_at,
            barriers: &barrier_plan,
            transfer: &self.transfer,
            first_write: &first_write,
            scale,
            uploaded: &uploaded,
            preloaded: &preloaded,
            native: native.as_ref(),
            queries: queries.as_deref(),
            prof: prof.as_ref(),
        };
        if !frame.gates.is_empty() && crate::diag("rendercond") && crate::diag::sampled() {
            crate::gatediag::frame(&sh);
        }
        let mut start = 0;
        let ranges: Vec<Range<usize>> = ends
            .iter()
            .map(|e| {
                let r = start..*e;
                start = *e;
                r
            })
            .collect();
        let items: Vec<(usize, &mut CommandBuffer, Range<usize>)> = rest
            .iter_mut()
            .zip(ranges)
            .enumerate()
            .map(|(i, (cb, r))| (i + 1, cb, r))
            .collect();
        mark(t0, "pre-chunks");
        let t_chunks = Instant::now();
        // Nothing more is encoded into the first command buffer in this case: the part before the first cut is committed as soon
        // as the thread that ends its last command buffer is done.
        if let Some(a) = &ahead {
            a.finish(0, first);
        }
        let results = jobs::run_all(self.threads, items, |(index, cb, range)| {
            let (start, cpu0) = (Instant::now(), nxkit_metal::thread::cpu_time_ns());
            let passes = range.len();
            let r = run_chunk(&sh, cb, &scratch, &keep, range);
            if let Some(a) = &ahead
                && index < a.count()
            {
                nxkit_metal::autorelease_pool(|| a.finish(index, cb));
            }
            let timing = ChunkTiming {
                delay_ns: nanos(t_chunks, start),
                wall_ns: nanos(start, Instant::now()),
                cpu_ns: nxkit_metal::thread::cpu_time_ns().saturating_sub(cpu0),
                passes,
            };
            (r, timing)
        });
        let chunks_ns = nanos(t_chunks, Instant::now());
        let mut copied_back = Vec::new();
        let mut timings = Vec::with_capacity(results.len());
        for ((s, copied), timing) in results {
            stats.add(&s);
            copied_back.extend(copied);
            timings.push(timing);
        }
        CHUNKS.with(|c| *c.borrow_mut() = timings);
        let (mirrored, mirrored_ranges) = match rest.last_mut() {
            Some(cb) if !dirty.is_empty() => {
                let t_cb = Instant::now();
                let r = self.copy_back_written(
                    cb,
                    &scratch,
                    view,
                    &textures,
                    &dirty,
                    &copied_back,
                    (pools.as_ref(), &mut stats),
                    prof.as_ref(),
                );
                PHASES.with(|p| p.borrow_mut()[1] = nanos(t_cb, Instant::now()));
                r
            }
            _ => Default::default(),
        };

        // And their words that the GPU wrote go back into guest memory after the last pass.
        let t_merge = crate::diag("rename").then(Instant::now);
        if let (Some(table), Some(cb)) = (&merge_table, rest.last_mut())
            && crate::rename::merge_encode(
                &self.pipelines,
                cb,
                table,
                &scratch,
                &merges,
                crate::rename::Phase::Out,
            )
            .is_err()
        {
            stats.skip(Skip::Encode);
        }
        if let Some(t) = t_merge {
            self.rename.cost_add(7, nanos(t, Instant::now()));
        }
        if let Some(p) = prof {
            crate::passprof::end_frame(p);
        }
        stats.chunks_ns = chunks_ns;
        stats.barriers_emitted = barrier_plan.emitted;
        stats.barriers_avoided = barrier_plan.avoided;
        stats.command_buffers = 1 + ends.len() as u32;
        stats.pipeline_builds = self.pipelines.counts().0.saturating_sub(builds_before) as u32;
        mark(t0, "chunk-end");
        stats.encode_ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        hitch_diag(&stats, detail.draws.len());

        // `privlatestage`: the pages the CPU wrote since the flush are copied into the mirror again, at the end of the first
        // command buffer, which runs before the others.
        if let Some(o) = pools.overlay() {
            let jobs = crate::private::late_stage(o);
            if !jobs.is_empty() {
                let ok = nxkit_metal::ArgumentTable::new(&self.ctx, 31, 0).ok().and_then(|t| {
                    crate::private::encode_copy_in(&self.pipelines, first, &t, &scratch, &jobs).ok()
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
                .and_then(|t| crate::private::encode_copy_in(&self.pipelines, cb, &t, &scratch, end).ok());
            match ok {
                Some(_) => crate::private::late_copies_encoded(o.frame(), *start_at, *end_at),
                None => stats.skip(Skip::Encode),
            }
        }
        // ICBs and argument buffers made during the encode join the residency set before the commit.
        let t_commit = Instant::now();
        self.ctx.commit_residency();
        let t_submit = Instant::now();
        let submitted = batch.submit()?;
        crate::private::note_split(&early, plan.passes.len(), ahead.is_some());
        PHASES.with(|p| {
            let mut p = p.borrow_mut();
            p[2] = nanos(t_commit, t_submit);
            p[3] = nanos(t_submit, Instant::now());
        });
        let number = submitted.number();
        self.older = self.last.replace(submitted);
        self.copyback.submitted(number, &mirrored_ranges);
        if lum || scan {
            let _ = self.wait();
        }
        if lum {
            crate::lum::dump(&self.ctx);
        }
        if scan {
            crate::nanscan::dump();
        }
        if let Some(q) = queries.as_ref().filter(|q| q.has_mesh()) {
            let _ = self.wait();
            q.mesh_print(&self.guest, &|a| crate::meshvis::Where {
                table: self.gmem.range_of(a),
                written: self.gmem.learned_range_of(a),
            });
        }
        if crate::diag("tiny") && crate::diag::sampled() {
            let _ = self.wait();
            self.tiny_frame_end(view, &textures, &mirrored);
        }
        crate::health::frame(diag_frame, &stats);
        Ok(EncodeReport {
            batch: number,
            deswizzled,
            gpu_wrote,
            copied_back,
            mirrored,
            stats,
            queries,
        })
    }
}

impl Gpu {
    /// `NXKIT_DIAG=tiny` at the end of a sampled frame, after the GPU finished: the guest bytes of the tiny textures
    /// that were copied back, and the ranges of `NXKIT_DIAG_WATCH`.
    fn tiny_frame_end(
        &self,
        view: &dyn EncodeView,
        textures: &FastMap<TexId, Res>,
        mirrored: &[TexId],
    ) {
        let base = self.guest.address_map().gpu_base();
        let read = |addr: u64, len: usize| {
            let mut buf = vec![0u8; len];
            self.guest
                .read(addr.wrapping_sub(base), &mut buf)
                .ok()
                .map(|()| buf)
        };
        for id in mirrored {
            let (Some(res), Ok(rec)) = (textures.get(id), view.record(*id)) else {
                continue;
            };
            if rec.layout.size == 0
                || rec.layout.size > crate::diag::TINY_MAX
                || !crate::diag::tiny_line()
            {
                continue;
            }
            if let Some(buf) = read(rec.address.get(), rec.layout.size as usize) {
                eprintln!(
                    "DIAG tiny copyback {id:?} {:?} {}x{} at 0x{:x} {}",
                    res.desc.format,
                    res.desc.width,
                    res.desc.height,
                    rec.address.get(),
                    crate::diag::tiny_words(&buf)
                );
            }
        }
        for &(addr, len) in crate::diag::watch_ranges() {
            if let Some(buf) = read(addr, len) {
                eprintln!(
                    "DIAG tiny watch 0x{addr:x} {}",
                    crate::diag::tiny_words(&buf)
                );
            }
        }
    }
}

/// `NXKIT_DIAG=passprof`: what a pass is, for the report.
fn describe_pass(
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[crate::draws::Ordinal],
    textures: &FastMap<TexId, Res>,
    i: usize,
) -> String {
    use crate::detail::CopyKind;
    let p = &plan.passes[i];
    let n = p.cmds.len();
    let cmds = || {
        p.cmds
            .clone()
            .filter_map(|c| Some((frame.cmds.get(c as usize)?, ords.get(c as usize).copied()?)))
    };
    match p.kind {
        PassKind::Render => {
            let what = plan
                .attachments_of(p)
                .first()
                .and_then(|a| textures.get(&a.tex))
                .map_or_else(String::new, |r| {
                    format!(" {}x{} {:?}", r.desc.width, r.desc.height, r.desc.format)
                });
            let draws = cmds().filter(|(c, _)| matches!(c, Cmd::Draw { .. }));
            let count = draws.clone().count();
            let fs = draws
                .clone()
                .next()
                .and_then(|(_, o)| detail.draws.get(o.draw as usize))
                .map_or_else(
                    || "none".to_string(),
                    |d| {
                        d.program
                            .fragment
                            .as_deref()
                            .map_or_else(|| "depth-only".to_string(), str::to_string)
                    },
                );
            format!("render cmds {n} draws {count} fs {fs}{what}")
        }
        PassKind::Compute => {
            let ds: Vec<String> = cmds()
                .filter(|(c, _)| matches!(c, Cmd::Dispatch { .. }))
                .map(|(_, o)| {
                    detail.dispatches.get(o.dispatch as usize).map_or_else(
                        || "?".to_string(),
                        |d| {
                            let [x, y, z] = d.groups;
                            match d.indirect {
                                Some(a) => format!("{} indirect@{a:#x}", d.program),
                                None => format!("{} {x}x{y}x{z}", d.program),
                            }
                        },
                    )
                })
                .collect();
            let more = ds.len().saturating_sub(3);
            let mut s = format!("compute cmds {n} [{}]", ds.iter().take(3).cloned().collect::<Vec<_>>().join("; "));
            if more > 0 {
                s.push_str(&format!(" +{more}"));
            }
            s
        }
        PassKind::Copy => {
            let items: Vec<String> = cmds()
                .map(|(c, o)| {
                    let d = detail.copies.get(o.copy as usize);
                    let [w, h, dp] = d.map_or([0; 3], |d| d.size);
                    match (c, d.map(|d| d.kind)) {
                        (Cmd::Copy { src, dst }, k) => format!(
                            "{} {w}x{h}x{dp} {src:?}->{dst:?}",
                            match k {
                                Some(CopyKind::TextureToTexture) => "tex-copy",
                                Some(CopyKind::BufferToTexture) => "buf-to-tex",
                                Some(CopyKind::TextureToBuffer) => "tex-to-buf",
                                Some(CopyKind::Clear) => "clear",
                                _ => "copy",
                            }
                        ),
                        (Cmd::BufferCopy, _) => d.map_or_else(
                            || "buffer".to_string(),
                            |d| match d.kind {
                                CopyKind::BufferFill => {
                                    format!("fill {} bytes @{:#x}", d.bytes, d.dst_addr)
                                }
                                _ => format!(
                                    "buffer-copy {} bytes {:#x}->{:#x}",
                                    d.bytes, d.src_addr, d.dst_addr
                                ),
                            },
                        ),
                        (Cmd::Downsample { src, dst }, _) => {
                            format!("downsample {w}x{h} {src:?}->{dst:?}")
                        }
                        _ => "other".to_string(),
                    }
                })
                .collect();
            let more = items.len().saturating_sub(3);
            let mut s = format!("copy cmds {n} [{}]", items.iter().take(3).cloned().collect::<Vec<_>>().join("; "));
            if more > 0 {
                s.push_str(&format!(" +{more}"));
            }
            s
        }
        PassKind::CpuRead => {
            let items: Vec<String> = cmds()
                .filter_map(|(c, _)| match c {
                    Cmd::CpuRead(id) => Some(match textures.get(id) {
                        Some(r) => format!(
                            "{id:?} {}x{} {:?}",
                            r.desc.width, r.desc.height, r.desc.format
                        ),
                        None => format!("{id:?}"),
                    }),
                    _ => None,
                })
                .collect();
            format!("cpuread [{}]", items.join("; "))
        }
        _ => format!("other cmds {n}"),
    }
}

#[cfg(test)]
mod lead_tests {
    use super::*;

    #[test]
    fn a_lead_chunk_takes_the_first_eighth_of_the_weight_and_the_chunks_keep_their_ends() {
        let weights = vec![10u32; 40];
        let mut ends = jobs::split(&weights, 4);
        assert_eq!(ends, vec![10, 20, 30, 40]);
        assert!(lead_chunk(&weights, &mut ends, 8));
        assert_eq!(ends, vec![5, 10, 20, 30, 40]);
    }

    #[test]
    fn no_lead_chunk_for_a_short_frame_one_chunk_a_full_ring_or_a_big_first_pass() {
        let weights = vec![10u32; 40];
        let mut one = vec![40];
        assert!(!lead_chunk(&weights, &mut one, 8));
        assert_eq!(one, vec![40]);
        let mut ends = jobs::split(&weights, 4);
        assert!(!lead_chunk(&weights, &mut ends, 5), "1 + 5 command buffers do not fit 5");
        assert_eq!(ends.len(), 4);
        let short = vec![10u32; 4];
        let mut e = jobs::split(&short, 2);
        assert!(!lead_chunk(&short, &mut e, 8));
        let heavy = [1000u32, 1, 1, 1, 1, 1, 1, 1, 1, 1];
        let mut e = vec![1, 6, 10];
        assert!(!lead_chunk(&heavy, &mut e, 8), "the first chunk is the lead already");
        assert_eq!(e, vec![1, 6, 10]);
    }
}
