//! The resolution of the texture handles of a frame and the copy-back of what the GPU wrote.

use super::*;

impl Gpu {
    pub(super) fn resolve_handles(
        &mut self,
        view: &dyn EncodeView,
        detail: &FrameDetail,
        pool_ids: &[u32],
        scale: f32,
        textures: &mut FastMap<TexId, Res>,
        stats: &mut EncodeStats,
    ) -> Handles {
        let mut out = Handles::default();
        let wanted = detail
            .draws
            .iter()
            .flat_map(|d| d.textures.iter())
            .chain(detail.dispatches.iter().flat_map(|d| d.textures.iter()))
            .map(|t| t.handle)
            // Handle 0 is a slot the game left empty. The binding reads it as the null texture.
            .filter(|&h| h != 0);
        // Each handle is resolved once a frame, also when it fails: a handle that does not resolve is bound by many
        // draws, and trying it again for each of them cost a millisecond in heavy frames.
        let mut seen: FastSet<u64> = FastSet::default();
        for handle in wanted {
            if !seen.insert(handle) {
                continue;
            }
            match self.resolve_one(view, handle, scale, textures, stats) {
                Ok((r, owner, view)) => {
                    self.bindless.publish(handle, r, owner);
                    out.map.insert(handle, r);
                    out.keep.push(view);
                }
                Err(why) => stats.skip(why),
            }
        }
        self.resolve_pool(view, pool_ids, scale, textures, &mut out, stats);
        out
    }

    /// Reads the bitmap of the texture pool, writes the head of the tables and the samplers, and returns the texture
    /// ids to fill for the frame: the ids that shaders marked, when a draw or dispatch of the frame has a `tex_pool`
    /// binding, else none. A snapshot lists the ids that were wanted when the front end captured it.
    pub(super) fn pool_prepare(
        &mut self,
        view: &dyn EncodeView,
        detail: &FrameDetail,
        stats: &mut EncodeStats,
    ) -> Vec<u32> {
        let limit = view.texture_limit();
        if let Some(limit) = limit {
            self.bindless.raise_texture_limit(limit);
        }
        let scanned = limit.map_or_else(Vec::new, |l| self.bindless.wanted(l));
        self.pool_hint.set_ids(&scanned);
        self.fill_samplers(view, stats);
        // A texture of an element may be gone since the last frame (`nvnTextureFinalize`, eviction).
        self.pool_release();
        if scanned.is_empty() && view.pool_ids().is_none_or(<[u32]>::is_empty) {
            return Vec::new();
        }
        if !self.frame_reads_pool(detail) {
            return Vec::new();
        }
        let ids = view.pool_ids().map_or(scanned, <[u32]>::to_vec);
        nxkit_nvn::diag::note_n(nxkit_nvn::diag::Reason::PoolWanted, ids.len() as u64);
        ids
    }

    /// Sets to 0 the elements of the texture table whose Metal texture the store dropped or replaced.
    pub(super) fn pool_release(&mut self) {
        let store = &self.store;
        let cleared = self
            .bindless
            .release_gone(|t, generation| store.get(t).map(|r| r.generation) != Some(generation));
        nxkit_nvn::diag::note_n(nxkit_nvn::diag::Reason::PoolCleared, u64::from(cleared));
    }

    /// Writes the element of every registered sampler into the sampler table, when a sampler was registered since the
    /// last time. A sampler read is not marked, so the host fills them all.
    fn fill_samplers(&mut self, view: &dyn EncodeView, stats: &mut EncodeStats) {
        let epoch = view.sampler_epoch();
        if !self.pool_hint.needs_samplers(epoch) {
            return;
        }
        let mut complete = true;
        for (id, tsc) in view.samplers() {
            match self.bindless.sampler(Some(&tsc)) {
                Ok(resource) => self.bindless.publish_sampler(id, resource),
                Err(why) => {
                    complete = false;
                    stats.skip(why);
                }
            }
        }
        if complete {
            self.pool_hint.set_sampler_epoch(epoch);
        }
    }

    /// True when a draw or dispatch of the frame runs a shader that has a `tex_pool` binding.
    fn frame_reads_pool(&mut self, detail: &FrameDetail) -> bool {
        let mut checked: FastSet<(&str, bool)> = FastSet::default();
        let mut keys: Vec<(&str, bool)> = Vec::new();
        for d in &detail.draws {
            let p = &d.program;
            keys.push((&p.vertex, true));
            keys.extend(p.fragment.as_deref().map(|k| (k, false)));
            keys.extend(p.task.as_deref().map(|k| (k, false)));
            keys.extend(p.mesh.as_deref().map(|k| (k, false)));
        }
        keys.extend(
            detail
                .dispatches
                .iter()
                .map(|d| (d.program.as_str(), false)),
        );
        for (key, vertex) in keys {
            if key.is_empty() || !checked.insert((key, vertex)) {
                continue;
            }
            let map = &mut self.pool_decl[usize::from(vertex)];
            let has = match map.get(key) {
                Some(has) => *has,
                None => {
                    let has = self.programs.stage(key, vertex).is_ok_and(|p| {
                        p.binds
                            .iter()
                            .any(|b| b.kind == crate::programs::BindKind::TexPool)
                    });
                    map.insert(key.to_owned(), has);
                    has
                }
            };
            if has {
                return true;
            }
        }
        false
    }

    /// Fills the element of each wanted texture id, after the bound handles. An id that names no live texture gets 0.
    fn resolve_pool(
        &mut self,
        view: &dyn EncodeView,
        ids: &[u32],
        scale: f32,
        textures: &mut FastMap<TexId, Res>,
        out: &mut Handles,
        stats: &mut EncodeStats,
    ) {
        use nxkit_nvn::diag::{Reason, note, note_n};
        let (mut filled, mut unresolved) = (0u64, 0u64);
        for &id in ids {
            // Descriptor 0 is the unbound slot: its element stays the null texture.
            if id == 0 {
                continue;
            }
            match self.resolve_pool_one(view, id, scale, textures, stats) {
                Ok(Some((tex, owner))) => {
                    filled += u64::from(self.bindless.publish_texture(
                        id,
                        tex.gpu_resource_id(),
                        owner,
                    ));
                    out.keep.push(tex);
                }
                Ok(None) => {
                    self.bindless.clear_texture(id);
                    if unresolved == 0 {
                        note(Reason::PoolUnresolved, format_args!("texture id {id}"));
                    }
                    unresolved += 1;
                }
                Err(why) => {
                    self.bindless.clear_texture(id);
                    stats.skip(why);
                }
            }
        }
        note_n(Reason::PoolFilled, filled);
        note_n(Reason::PoolUnresolved, unresolved.saturating_sub(1));
    }

    /// The view of descriptor `id` and the owner of its element, or `None` when the id has no live texture.
    #[allow(clippy::type_complexity)]
    fn resolve_pool_one(
        &mut self,
        view: &dyn EncodeView,
        id: u32,
        scale: f32,
        textures: &mut FastMap<TexId, Res>,
        stats: &mut EncodeStats,
    ) -> std::result::Result<Option<(Texture, (TexId, u64))>, Skip> {
        let Ok(slot) = view.texture_slot(id) else {
            return Ok(None);
        };
        let tex = slot.texture;
        if let std::collections::hash_map::Entry::Vacant(e) = textures.entry(tex) {
            let Ok(rec) = view.record(tex) else {
                return Ok(None);
            };
            let res = self.store.ensure(rec, scale, Use::default(), stats)?;
            e.insert(res);
        }
        let res = textures.get(&tex).ok_or(Skip::Texture)?;
        let tex_view = self.store.view(tex, res, &slot.header, stats)?;
        Ok(Some((tex_view, (tex, res.generation))))
    }

    /// Makes the views that the shaders of the frame declare for their texture bindings when they differ from the
    /// view of the descriptor (a cube read as a cube array, a 2D array read as 2D). Metal reads a texture of another
    /// type than the argument declares as one fixed value.
    pub(super) fn fit_handles(
        &mut self,
        view: &dyn EncodeView,
        detail: &FrameDetail,
        textures: &FastMap<TexId, Res>,
        handles: &mut Handles,
        stats: &mut EncodeStats,
    ) {
        let mut needed: FastSet<(u64, nxkit_metal::TextureKind)> = FastSet::default();
        let stages = detail
            .draws
            .iter()
            .flat_map(|d| {
                [
                    (
                        Some(&*d.program.vertex).filter(|k| !k.is_empty()),
                        true,
                        crate::detail::Stage::Vertex,
                        &d.textures,
                    ),
                    (
                        d.program.fragment.as_deref(),
                        false,
                        crate::detail::Stage::Fragment,
                        &d.textures,
                    ),
                    (
                        d.program.task.as_deref(),
                        false,
                        crate::detail::Stage::Task,
                        &d.textures,
                    ),
                    (
                        d.program.mesh.as_deref(),
                        false,
                        crate::detail::Stage::Mesh,
                        &d.textures,
                    ),
                ]
            })
            .chain(detail.dispatches.iter().map(|d| {
                (
                    Some(d.program.as_str()),
                    false,
                    crate::detail::Stage::Compute,
                    &d.textures,
                )
            }));
        for (key, vertex, stage, binds) in stages {
            let Some(key) = key else { continue };
            if binds.is_empty() {
                continue;
            }
            for &(reg, k) in declared_views(&mut self.fit_decl, &self.programs, key, vertex) {
                let Some(h) = binds
                    .iter()
                    .find(|t| t.stage == stage && t.slot == reg)
                    .map(|t| t.handle)
                else {
                    continue;
                };
                if h != 0 && handles.map.contains_key(&h) {
                    needed.insert((h, k));
                }
            }
        }
        for (h, k) in needed {
            let Ok((slot, _)) = view.resolve(h) else {
                continue;
            };
            let Some(res) = textures.get(&slot.texture) else {
                continue;
            };
            let Ok(base) = self.store.view(slot.texture, res, &slot.header, stats) else {
                continue;
            };
            if base.desc().kind == k {
                continue;
            }
            if let Ok(t) = self
                .store
                .view_as(slot.texture, res, &slot.header, Some(k), stats)
            {
                static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if crate::diag("fit") && crate::diag::first(&SHOWN, 300) {
                    eprintln!(
                        "DIAG fit handle {h:#x} {:?} {:?} -> {k:?}",
                        slot.texture,
                        base.desc().kind
                    );
                }
                handles.fits.insert((h, k), t.gpu_resource_id());
                handles.keep.push(t);
            }
        }
    }

    /// The `stale` category: hashes the guest bytes of a texture every frame and prints when they changed, with the
    /// residency that the front end has for it. A change while the texture is not `GuestNewer` is a CPU write that the
    /// front end did not see. It reads the whole texture, so `NXKIT_DIAG_TEX` should narrow it down.
    pub(super) fn stale_diag(
        &self,
        id: TexId,
        rec: &nxkit_nvn::resource::TextureRecord,
        res: &Res,
    ) {
        use std::hash::{Hash, Hasher};
        static SEEN: Mutex<Option<FastMap<u64, u64>>> = Mutex::new(None);
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let mut buf = vec![0u8; rec.layout.size as usize];
        let off = rec
            .address
            .get()
            .wrapping_sub(self.guest.address_map().gpu_base());
        if self.guest.read(off, &mut buf).is_err() {
            return;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        buf.hash(&mut h);
        let h = h.finish();
        let mut g = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
        let key = rec.address.get();
        if g.get_or_insert_with(FastMap::default)
            .insert(key, h)
            .is_some_and(|old| old != h)
            && crate::diag::first(&SHOWN, 200)
        {
            eprintln!(
                "DIAG stale {id:?} {:?} {}x{} addr {key:#x} changed, residency {:?} needs upload {}",
                res.desc.format,
                res.desc.width,
                res.desc.height,
                rec.residency,
                TextureStore::needs_upload(res, rec)
            );
        }
    }

    fn resolve_one(
        &mut self,
        view: &dyn EncodeView,
        handle: u64,
        scale: f32,
        textures: &mut FastMap<TexId, Res>,
        stats: &mut EncodeStats,
    ) -> std::result::Result<(Resolved, (TexId, u64), Texture), Skip> {
        let (slot, tsc) = view.resolve(handle).map_err(|e| {
            let what = format_args!("handle {handle:#x}: {e:?}");
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::HandleUnresolved, what);
            crate::tex_skip(what)
        })?;
        let id = slot.texture;
        if let std::collections::hash_map::Entry::Vacant(e) = textures.entry(id) {
            let rec = view.record(id).map_err(|e| {
                let what = format_args!("handle {handle:#x} tex {id:?}: {e:?}");
                nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::HandleUnresolved, what);
                crate::tex_skip(what)
            })?;
            let res = self.store.ensure(rec, scale, Use::default(), stats)?;
            e.insert(res);
        }
        let res = textures.get(&id).ok_or(Skip::Texture)?;
        let tex_view = self.store.view(id, res, &slot.header, stats)?;
        if crate::diag("barr") || crate::diag("nanscan") {
            crate::barr::note(handle, id);
        }
        if crate::diag("bc") && res.desc.format.is_compressed() {
            static SEEN_BC: std::sync::Mutex<Vec<TexId>> = std::sync::Mutex::new(Vec::new());
            if let (Ok(rec), Ok(mut seen)) = (view.record(id), SEEN_BC.lock())
                && !seen.contains(&id)
                && seen.len() < 200
            {
                seen.push(id);
                let map = self.guest.address_map();
                let mut head = vec![0u8; 32.min(rec.layout.size as usize)];
                let read = rec
                    .address
                    .get()
                    .checked_sub(map.gpu_base())
                    .is_some_and(|off| self.guest.read(off, &mut head).is_ok());
                eprintln!(
                    "DIAG bc {id:?} {}x{} {:?} residency {:?} size {} guest_read {read} head {head:02x?}",
                    res.desc.width,
                    res.desc.height,
                    res.desc.format,
                    rec.residency,
                    rec.layout.size
                );
            }
        }
        if crate::diag("hnd") {
            static SEEN: std::sync::Mutex<Vec<(u64, TexId, u64)>> =
                std::sync::Mutex::new(Vec::new());
            let key = (handle, id, 0u64);
            if let Ok(mut seen) = SEEN.lock()
                && !seen.contains(&key)
                && seen.len() < 2000
            {
                seen.push(key);
                let v = tex_view.desc();
                eprintln!(
                    "DIAG hnd {handle} {id:?} backing {}x{} {:?} scaled {} view {}x{} {:?} kind {:?}",
                    res.desc.width,
                    res.desc.height,
                    res.desc.format,
                    res.scaled,
                    v.width,
                    v.height,
                    v.format,
                    v.kind
                );
                if matches!(v.kind, nxkit_metal::TextureKind::Cube) {
                    eprintln!(
                        "DIAG hnd cube {handle} {id:?} swizzle {:?} view {:?} levels {}+{} backing_levels {} identity_view {}",
                        slot.header.view.swizzle,
                        slot.header.view.format,
                        slot.header.view.base_level,
                        slot.header.view.levels,
                        res.desc.mip_levels,
                        tex_view.gpu_resource_id() == res.tex.gpu_resource_id()
                    );
                }
            }
        }
        let sampler = self.bindless.sampler(tsc)?;
        Ok((
            Resolved {
                texture: tex_view.gpu_resource_id(),
                sampler,
                scaled: res.scaled,
            },
            (id, res.generation),
            tex_view,
        ))
    }

    /// Encodes, at the end of `cb` (the last command buffer of the frame), the copy-back of the written textures
    /// whose storage the CPU can access. The result is the textures copied back and their storage ranges.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn copy_back_written(
        &mut self,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        view: &dyn EncodeView,
        textures: &FastMap<TexId, Res>,
        dirty: &FastMap<TexId, (Dirty, usize)>,
        copied_back: &[TexId],
        (pools, stats): (&GuestPools, &mut EncodeStats),
        prof: Option<&crate::passprof::FrameProf>,
    ) -> (Vec<TexId>, Vec<(u64, u64)>) {
        // The last writer first: of two textures over the same storage, the one written last is copied back.
        let mut order: Vec<(usize, TexId)> = dirty
            .iter()
            .filter(|(_, (d, _))| !d.is_empty())
            .map(|(id, (_, last))| (*last, *id))
            .collect();
        order.sort_unstable_by(|a, b| b.cmp(a));
        let mut items = Vec::new();
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        let mut backings: FastSet<TexId> = FastSet::default();
        let (mut planned, mut over_budget) = (0u64, 0u32);
        for (_, id) in order {
            let (Some(res), Ok(rec)) = (textures.get(&id), view.record(id)) else {
                continue;
            };
            if res.desc.storage == nxkit_metal::Storage::Memoryless || copied_back.contains(&id) {
                continue;
            }
            let Some(flags) = self.copyback.candidate(id, rec) else {
                continue;
            };
            let (start, end) = (rec.address.get(), rec.end());
            if backings.contains(&res.barrier_id)
                || ranges.iter().any(|&(s, e)| s < end && start < e)
            {
                continue;
            }
            if planned + rec.layout.size > crate::copyback::budget() {
                over_budget += 1;
                nxkit_nvn::diag::note(
                    nxkit_nvn::diag::Reason::CopybackOverBudget,
                    format_args!(
                        "{id:?} {:?} {}x{} {} bytes",
                        res.desc.format, res.desc.width, res.desc.height, rec.layout.size
                    ),
                );
                continue;
            }
            planned += rec.layout.size;
            backings.insert(res.barrier_id);
            ranges.push((start, end));
            items.push((id, rec, res, &dirty[&id].0, flags));
        }
        let args: Vec<_> = items
            .iter()
            .map(|(_, rec, res, d, _)| (*rec, *res, *d))
            .collect();
        let stamp = |cp: &mut nxkit_metal::ComputePass<'_>| {
            if let Some(p) = prof {
                p.end_pass_compute(cp, p.extra_index());
            }
        };
        if let Some(p) = prof {
            p.set_extra(format!(
                "copyback-end {} textures {planned} bytes",
                items.len()
            ));
        }
        let results = self
            .transfer
            .copy_back_many_with(cb, scratch, pools, &args, Some(&stamp));
        let (mut done, mut done_ranges) = (Vec::new(), Vec::new());
        let (mut bytes, mut refused) = (0u64, 0u32);
        for ((id, rec, res, d, flags), r) in items.iter().zip(results) {
            match r {
                Ok(n) => {
                    stats.copybacks += 1;
                    stats.copyback_bytes += n;
                    bytes += n;
                    done.push(*id);
                    done_ranges.push((rec.address.get(), rec.end()));
                    self.barriers
                        .note_read(res.barrier_id, Stages::DISPATCH | Stages::BLIT);
                    self.copyback.log_first(*id, rec, res, *flags, d, n);
                }
                Err(why) => {
                    refused += 1;
                    stats.skip(why);
                    let what = format_args!(
                        "{id:?} {:?} {}x{} layers {} levels {} scaled {} widened {} linear {} stride {}: {why:?}",
                        res.desc.format,
                        res.desc.width,
                        res.desc.height,
                        res.desc.array_layers,
                        res.desc.mip_levels,
                        res.scaled,
                        res.widened,
                        rec.layout.linear,
                        rec.desc.stride
                    );
                    nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::CopybackRefused, what);
                    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    if crate::diag("copyback") && crate::diag::first(&N, 100) {
                        eprintln!("DIAG copyback refused {what}");
                    }
                }
            }
        }
        if !items.is_empty() {
            stats.compute_passes += 1;
        }
        self.copyback
            .log_frame(done.len() as u32, bytes, refused, over_budget);
        (done, done_ranges)
    }
}
