//! The encode of a frame: the passes cut into chunks, run on the threads and committed in order.

use super::*;

mod chunks;
mod passdiag;
mod steps;
#[cfg(test)]
mod tests;

use chunks::{lead_chunk, run_chunk, stream_enabled, weights_of};
use passdiag::describe_pass;

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
        let mut early = if fence != 0 && !plan.passes.is_empty() && crate::private::early_enabled()
        {
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
        let (rename_map, rename_buf, merges) = self.rename_begin(detail, pools, region);
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
        self.pin_copyback_targets(plan, view, &mut uses);
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
        let (first_write, gpu_wrote, dirty) =
            Self::track_writes(frame, plan, &textures, &pass_images);

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
            self.register_early_cuts(&mut early, &ends, &mut batch, fence, &dirty, frame, view);
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

        let (first, rest) = batch
            .buffers()
            .split_first_mut()
            .ok_or(MetalError::Invalid("the batch has no buffers"))?;
        first.retain(keep.clone());
        // `NXKIT_PRIVATE=1`: the pages that the CPU wrote since the frame before go into the mirror before anything of this
        // frame reads or writes it (the texture uploads below may read member pages).
        self.copy_in_pages(pools, first, &scratch, &mut stats);
        // Results one frame late: the GPU copies the late pages as the frame starts with them (after the copy-in) and,
        // below, as it ends, so that its completion hands the readers the frame's own bytes and knows which blocks it
        // changed.
        let late_copies = self.late_copies_start(pools, first, &scratch, &mut stats);
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
        let deswizzled = self.upload_textures(view, first, &scratch, pools, &textures, &mut stats);

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
        self.late_stage_end(pools, first, rest, &scratch, &late_copies, &mut stats);
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
