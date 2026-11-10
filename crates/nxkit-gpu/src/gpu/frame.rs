//! The encode of a frame: the passes cut into chunks, run on the threads and committed in order.

use super::*;

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
        let ends = if plan.passes.is_empty() {
            Vec::new()
        } else {
            let weights: Vec<u32> = plan
                .passes
                .iter()
                .map(|p| p.cmds.len() as u32 + 1)
                .collect();
            jobs::split(&weights, self.threads * CHUNKS_PER_THREAD)
        };
        let t_ring = Instant::now();
        let mut batch = self.ring.begin(1 + ends.len())?;
        PHASES.with(|p| p.borrow_mut()[0] = nanos(t_ring, Instant::now()));
        // The ring waited for the batch that used this slot before, so its write logs are final.
        let region = ((batch.number() - 1) % nxkit_metal::BATCHES_IN_FLIGHT as u64) as usize;
        self.gmem.learn(region);
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
            let tables = self.bindless.pool_buffers();
            let mut all: Vec<Texture> = textures.values().map(|r| r.tex.clone()).collect();
            all.extend(handles.keep);
            Arc::new((all, tables, gmem_buf.clone(), pools.clone()))
        };

        let mut deswizzled = Vec::new();
        let (first, rest) = batch
            .buffers()
            .split_first_mut()
            .ok_or(MetalError::Invalid("the batch has no buffers"))?;
        first.retain(keep.clone());
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
        let uploaded = Mutex::new(FastMap::default());
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
                pools: self.bindless.pools(),
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
        let items: Vec<(&mut CommandBuffer, Range<usize>)> = rest.iter_mut().zip(ranges).collect();
        mark(t0, "pre-chunks");
        let t_chunks = Instant::now();
        let results = jobs::run_all(self.threads, items, |(cb, range)| {
            let (start, cpu0) = (Instant::now(), nxkit_metal::thread::cpu_time_ns());
            let passes = range.len();
            let r = run_chunk(&sh, cb, &scratch, &keep, range);
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
                );
                PHASES.with(|p| p.borrow_mut()[1] = nanos(t_cb, Instant::now()));
                r
            }
            _ => Default::default(),
        };

        stats.chunks_ns = chunks_ns;
        stats.barriers_emitted = barrier_plan.emitted;
        stats.barriers_avoided = barrier_plan.avoided;
        stats.command_buffers = 1 + ends.len() as u32;
        stats.pipeline_builds = self.pipelines.counts().0.saturating_sub(builds_before) as u32;
        mark(t0, "chunk-end");
        stats.encode_ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        hitch_diag(&stats, detail.draws.len());

        // ICBs and argument buffers made during the encode join the residency set before the commit.
        let t_commit = Instant::now();
        self.ctx.commit_residency();
        let t_submit = Instant::now();
        let submitted = batch.submit()?;
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
