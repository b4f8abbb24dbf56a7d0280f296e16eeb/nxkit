//! The renamer: its waits, costs and the writes of a frame that it learns, and the ranges that other uses touch.

use super::*;

impl Cost {
    pub(super) fn add(&self, k: usize, ns: u64) {
        self.sum[k].fetch_add(ns, Ordering::Relaxed);
        self.max[k].fetch_max(ns, Ordering::Relaxed);
        self.calls[k].fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn take(&self) -> String {
        let ms = |ns: u64| ns as f64 / 1e6;
        COST_NAMES
            .iter()
            .enumerate()
            .map(|(k, name)| {
                let (sum, max, n) = (
                    self.sum[k].swap(0, Ordering::Relaxed),
                    self.max[k].swap(0, Ordering::Relaxed),
                    self.calls[k].swap(0, Ordering::Relaxed),
                );
                format!(
                    "{name} {:.3}/{:.3} ({n})",
                    ms(sum) / n.max(1) as f64,
                    ms(max)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl Renamer {
    pub(crate) fn new(ctx: &Context, guest: GuestSpace) -> Self {
        Self {
            ctx: ctx.clone(),
            guest,
            state: Mutex::new(State::default()),
            slots: Mutex::new((0..BATCHES_IN_FLIGHT).map(|_| Slot::default()).collect()),
            honest: AtomicU64::new(0),
            honest_why: AtomicU64::new(0),
            forced_mask: AtomicU64::new(0),
            forced: Default::default(),
            other_hashes: Mutex::new(FastMap::default()),
            watching: AtomicU64::new(0),
            waits: Default::default(),
            young_until: AtomicU64::new(0),
            cost: Cost::default(),
            wait_epoch: AtomicU64::new(0),
            chains: Mutex::new(FastMap::default()),
            active: AtomicBool::new(false),
        }
    }

    /// True when the wait for `fence` must be a full one: a range is judged in this frame, and its `post` bytes are read
    /// when the GPU finished exactly this frame.
    pub(crate) fn honest(&self, fence: u64) -> bool {
        fence != 0 && self.honest.load(Ordering::Acquire) == fence
    }

    /// The game called a wait for a fence: a frame flushed after it has a different epoch from one flushed before.
    pub(crate) fn note_wait(&self) {
        self.wait_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Counts a wait for the report of `NXKIT_DIAG=rename`: `kind` 0 honest because a range is learned, 1 because a range is
    /// checked, 2 honest for another reason, 3 overlapped.
    /// A wait for `fence` begins (`NXKIT_DIAG=rename`): in a frame whose ranges are judged, the bytes of the ranges are taken
    /// now, before the wait for the GPU, to find what the CPU wrote in flight.
    pub(crate) fn before_wait(&self, fence: u64) {
        self.timed(5, || self.before_wait_inner(fence));
    }

    fn before_wait_inner(&self, fence: u64) {
        if self.honest(fence) || (fence != 0 && self.watching.load(Ordering::Acquire) == fence) {
            lock(&self.state).before_wait(fence, &self.reader());
        }
    }

    pub(crate) fn count_wait(&self, fence: u64, overlapped: bool) {
        let k = if overlapped {
            3
        } else if self.honest(fence) {
            match self.honest_why.load(Ordering::Relaxed) {
                2 => 1,
                3 => 4,
                4 => 2,
                _ => 0,
            }
        } else {
            2
        };
        self.waits[k].fetch_add(1, Ordering::Relaxed);
        if !overlapped && self.honest(fence) {
            let m = self.forced_mask.load(Ordering::Relaxed);
            for (i, n) in self.forced.iter().enumerate() {
                if m >> i & 1 != 0 {
                    n.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// The knob `waitother`: whether a vertex, index or indirect buffer, or the source of a buffer copy, of at most 1 MiB
    /// has other bytes than at the flush before. The renamer does not cover these.
    pub(super) fn other_changed(
        &self,
        detail: &FrameDetail,
        read: &dyn Fn(u64, &mut [u8]) -> bool,
    ) -> bool {
        let mut now: FastMap<(u64, u64), u64> = FastMap::default();
        let mut changed = false;
        let mut prev = lock(&self.other_hashes);
        for (a, e) in unguarded(detail) {
            let len = e - a;
            if len == 0 || len > u64::from(SNAPSHOT_MAX_BYTES) || now.contains_key(&(a, len)) {
                continue;
            }
            let mut b = vec![0u8; len as usize];
            if !read(a, &mut b) {
                continue;
            }
            let h = b.chunks(8).fold(0xcbf2_9ce4_8422_2325u64, |h, c| {
                let mut w = [0u8; 8];
                w[..c.len()].copy_from_slice(c);
                (h ^ u64::from_le_bytes(w))
                    .wrapping_mul(0x100_0000_01b3)
                    .rotate_left(23)
            });
            if prev.get(&(a, len)).is_some_and(|p| *p != h) {
                changed = true;
            }
            now.insert((a, len), h);
        }
        *prev = now;
        changed
    }

    /// Runs `f`, and with `NXKIT_DIAG=rename` adds its time to entry point `k`.
    pub(super) fn timed<R>(&self, k: usize, f: impl FnOnce() -> R) -> R {
        if !crate::diag("rename") {
            return f();
        }
        let t = std::time::Instant::now();
        let r = f();
        self.cost.add(k, t.elapsed().as_nanos() as u64);
        r
    }

    /// Adds `ns` to entry point `k` of the cost report (the merge passes are encoded by the encoder).
    pub(crate) fn cost_add(&self, k: usize, ns: u64) {
        self.cost.add(k, ns);
    }

    pub(super) fn reader(&self) -> impl Fn(u64, &mut [u8]) -> bool + '_ {
        let base = self.guest.address_map().gpu_base();
        move |addr, buf| addr >= base && self.guest.read(addr - base, buf).is_ok()
    }

    /// Looks at the storage bindings of an encoded frame and adds those that a shader may write (`rw` of the binding) and
    /// that are small to the candidates. The bindings of a task or mesh stage are not looked at.
    pub(crate) fn note_rw(&self, detail: &FrameDetail, programs: &Programs) {
        self.timed(3, || {
            self.note_rw_with(
                detail,
                detail.rename_check.as_deref(),
                &|key, vertex, slot| {
                    programs.stage(key, vertex).is_ok_and(|p| {
                        p.binds
                            .iter()
                            .any(|b| b.kind == BindKind::Sbuf && b.reg == slot && b.rw)
                    })
                },
            );
        });
    }

    /// [`Renamer::note_rw`] with `rw_of(program key, vertex, slot)` saying whether the program may write the binding.
    ///
    /// A range that is a written candidate already needs nothing, so most bindings cost one lookup. The answer for a
    /// program is cached by the allocation of its key (the walker shares one per key) and the slot.
    pub(super) fn note_rw_with(
        &self,
        detail: &FrameDetail,
        check: Option<&[Recheck]>,
        rw_of: &dyn Fn(&str, bool, u32) -> bool,
    ) {
        use std::hash::BuildHasher;
        let mut st = lock(&self.state);
        st.watch = crate::diag("rename");
        if st.rw_ptr.len() > 50_000 {
            st.rw_ptr.clear();
        }
        if st.rw_str.len() > 50_000 {
            st.rw_str.clear();
        }
        // The same binding comes draw after draw.
        // A direct-mapped table of the bindings seen lately drops the repeats.
        let mut seen = vec![(0u64, 0u32, 0u32, 0usize); 4096];
        let mut look =
            |st: &mut State,
             key: Result<&Arc<str>, &str>,
             vertex: bool,
             s: &crate::detail::UboDetail| {
                let slot_stage = s.slot | (s.stage as u32) << 24;
                let id = match key {
                    Ok(k) => Arc::as_ptr(k).cast::<u8>() as usize,
                    Err(k) => k.as_ptr() as usize,
                };
                if s.size == 0 || s.size > SNAPSHOT_MAX_BYTES || s.address == 0 {
                    return;
                }
                let at = ((s.address >> 4) ^ (s.address >> 16) ^ id as u64 ^ u64::from(slot_stage))
                    as usize
                    & 4095;
                if seen[at] == (s.address, s.size, slot_stage, id) {
                    return;
                }
                seen[at] = (s.address, s.size, slot_stage, id);
                if st.cands.get(&s.address).is_some_and(|c| {
                    !c.ro && s.size <= c.size && c.learn.class() != Class::Snapshot
                }) {
                    return;
                }
                let rw = match key {
                    Ok(k) => {
                        let id = (Arc::as_ptr(k).cast::<u8>() as usize, slot_stage);
                        match st.rw_ptr.get(&id) {
                            Some((_, rw)) => *rw,
                            None => {
                                let rw = rw_of(k, vertex, s.slot);
                                // The cache keeps the key alive, so its address names it for good.
                                st.rw_ptr.insert(id, (k.clone(), rw));
                                rw
                            }
                        }
                    }
                    Err(k) => {
                        let id = (
                            foldhash::fast::FixedState::default().hash_one(k),
                            slot_stage,
                        );
                        *st.rw_str
                            .entry(id)
                            .or_insert_with(|| rw_of(k, vertex, s.slot))
                    }
                };
                if st.rw_known.len() > 100_000 {
                    st.rw_known.clear();
                }
                let prog = match key {
                    Ok(k) => st.prog_of(k),
                    Err(k) => foldhash::fast::FixedState::default().hash_one(k),
                };
                st.rw_known.insert(
                    Key {
                        prog,
                        slot_stage,
                        size: s.size.next_multiple_of(256),
                    },
                    rw,
                );
                if rw && s.size <= MAX_BYTES {
                    st.candidate(s.address, s.size);
                } else if rw {
                    // Too large to rename; a snapshot that is written is state.
                    st.demote(s.address);
                } else {
                    st.watch_read_only(s.address, s.size);
                }
            };
        let mut visit = |st: &mut State, kind: u8, i: usize, j: usize| {
            if kind == 0 {
                let Some(d) = detail.draws.get(i) else { return };
                if d.args.is_mesh() {
                    return;
                }
                let Some(s) = d.sbufs.get(j) else { return };
                match s.stage {
                    Stage::Vertex => look(st, Ok(&d.program.vertex), true, s),
                    Stage::Fragment => {
                        if let Some(k) = &d.program.fragment {
                            look(st, Ok(k), false, s);
                        }
                    }
                    _ => {}
                }
            } else if let Some(d) = detail.dispatches.get(i)
                && let Some(s) = d.sbufs.get(j)
                && s.stage == Stage::Compute
            {
                look(st, Err(&d.program), false, s);
            }
        };
        match check {
            // Only the bindings that the flush could not place.
            Some(list) => {
                for &(kind, i, j) in list {
                    visit(&mut st, kind, i as usize, j as usize);
                }
            }
            None => {
                for (i, d) in detail.draws.iter().enumerate() {
                    for j in 0..d.sbufs.len() {
                        visit(&mut st, 0, i, j);
                    }
                }
                for (i, d) in detail.dispatches.iter().enumerate() {
                    for j in 0..d.sbufs.len() {
                        visit(&mut st, 1, i, j);
                    }
                }
            }
        }
    }
}

/// The words of `bytes` that are not 0, as `i:hex`, at most 24.
pub(super) fn nonzero_words(bytes: &[u8]) -> String {
    let v: Vec<String> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let w = u32::from_le_bytes(*w);
            (w != 0).then(|| format!("{i}:{w:#x}"))
        })
        .take(24)
        .collect();
    v.join(" ")
}

/// The buffers that a frame reads and that nothing in the renamer covers: vertex, index and indirect buffers and the
/// source of buffer copies, as `(start, end)`.
fn unguarded(detail: &FrameDetail) -> Vec<(u64, u64)> {
    let mut v = Vec::new();
    let mut add = |addr: u64, len: u64| {
        if addr != 0 {
            v.push((addr, addr.saturating_add(len)));
        }
    };
    for d in &detail.draws {
        let verts = match d.args {
            DrawArgs::Arrays { first, count, .. } => Some(u64::from(first) + u64::from(count)),
            _ => None,
        };
        for st in &d.streams {
            let used = verts.map_or(UNKNOWN_BYTES, |n| n * u64::from(st.stride.max(1)));
            add(st.address, st.size.min(used).min(UNKNOWN_BYTES));
        }
        match d.args {
            DrawArgs::Indexed {
                index_type,
                address,
                count,
                ..
            } => add(address, u64::from(count) << index_type.min(2)),
            DrawArgs::ArraysIndirect { indirect } | DrawArgs::MeshTasksIndirect { indirect } => {
                add(indirect, 32);
            }
            DrawArgs::IndexedIndirect {
                address, indirect, ..
            } => {
                add(address, UNKNOWN_BYTES);
                add(indirect, 32);
            }
            _ => {}
        }
    }
    for d in &detail.dispatches {
        if let Some(a) = d.indirect {
            add(a, 12);
        }
    }
    for c in &detail.copies {
        let len = if c.bytes != 0 { c.bytes } else { UNKNOWN_BYTES };
        match c.kind {
            crate::detail::CopyKind::BufferToTexture => add(c.buffer, len),
            crate::detail::CopyKind::BufferToBuffer => add(c.src_addr, len),
            _ => {}
        }
    }
    v
}

/// The ranges that copies and fills of the frame write.
pub(super) fn copy_writes(detail: &FrameDetail) -> Vec<(u64, u64)> {
    use crate::detail::CopyKind;
    detail
        .copies
        .iter()
        .filter_map(|c| {
            let len = if c.bytes != 0 { c.bytes } else { UNKNOWN_BYTES };
            let a = match c.kind {
                CopyKind::BufferFill | CopyKind::BufferToBuffer => c.dst_addr,
                CopyKind::TextureToBuffer => c.buffer,
                _ => 0,
            };
            (a != 0).then_some((a, a.saturating_add(len)))
        })
        .collect()
}

/// The ranges that a frame reads or writes in ways other than a storage binding: uniform buffers, vertex, index and
/// indirect buffers, and the buffer sides of copies and fills.
pub(super) fn others(detail: &FrameDetail) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = Vec::new();
    // A direct-mapped table of the uses seen lately drops the repeats: draw after draw binds the same buffers.
    let mut seen = vec![(0u64, 0u64); 4096];
    let mut add = |addr: u64, len: u64| {
        let slot = ((addr >> 4) ^ (addr >> 16) ^ len) as usize & 4095;
        if addr != 0 && seen[slot] != (addr, len) {
            seen[slot] = (addr, len);
            v.push((addr, addr.saturating_add(len.max(1))));
        }
    };
    for d in &detail.draws {
        for u in &d.ubos {
            add(u.address, u64::from(u.size));
        }
        let verts = match d.args {
            DrawArgs::Arrays { first, count, .. } => Some(u64::from(first) + u64::from(count)),
            _ => None,
        };
        for st in &d.streams {
            let used = verts.map_or(UNKNOWN_BYTES, |n| n * u64::from(st.stride.max(1)));
            add(st.address, st.size.min(used).min(UNKNOWN_BYTES));
        }
        match d.args {
            DrawArgs::Indexed {
                index_type,
                address,
                count,
                ..
            } => add(address, u64::from(count) << index_type.min(2)),
            DrawArgs::ArraysIndirect { indirect } | DrawArgs::MeshTasksIndirect { indirect } => {
                add(indirect, 32);
            }
            DrawArgs::IndexedIndirect {
                address, indirect, ..
            } => {
                add(address, UNKNOWN_BYTES);
                add(indirect, 32);
            }
            DrawArgs::MultiIndirectCount {
                index,
                indirect,
                count,
                max,
                stride,
            } => {
                if let Some((_, a)) = index {
                    add(a, UNKNOWN_BYTES);
                }
                add(indirect, u64::from(max).saturating_mul(stride.max(32)));
                add(count, 4);
            }
            _ => {}
        }
    }
    for d in &detail.dispatches {
        for u in &d.ubos {
            add(u.address, u64::from(u.size));
        }
        if let Some(a) = d.indirect {
            add(a, 12);
        }
    }
    for c in &detail.copies {
        let len = if c.bytes != 0 { c.bytes } else { UNKNOWN_BYTES };
        for a in [c.buffer, c.src_addr, c.dst_addr] {
            add(a, len);
        }
    }
    v
}
