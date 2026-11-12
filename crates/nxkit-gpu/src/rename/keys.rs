//! The state's bindings, keys and candidates.

use super::*;

impl State {
    /// The hash of the program key `k`, cached by its allocation.
    pub(super) fn prog_of(&mut self, k: &Arc<str>) -> u64 {
        use std::hash::BuildHasher;
        let id = Arc::as_ptr(k).cast::<u8>() as usize;
        if let Some((_, h)) = self.prog_hash.get(&id) {
            return *h;
        }
        if self.prog_hash.len() > 50_000 {
            self.prog_hash.clear();
        }
        let h = foldhash::fast::FixedState::default().hash_one(&**k);
        self.prog_hash.insert(id, (k.clone(), h));
        h
    }

    /// The storage bindings of a frame, each distinct binding once.
    pub(super) fn bindings(&mut self, detail: &FrameDetail) -> (Vec<Binding>, Vec<(u64, u32)>) {
        use std::hash::BuildHasher;
        let mut out = Vec::new();
        // The distinct ranges, sorted, for the checks of the frame.
        let mut ranges: Vec<(u64, u32)> = Vec::new();
        let mut seen_range = vec![(0u64, 0u32); 4096];
        // A direct-mapped table of the bindings seen lately drops the repeats: the same few hundred buffers are bound by
        // thousands of draws.
        let mut seen = vec![(0u64, 0u32, 0u32, 0u64); 4096];
        let mut push = |s: &crate::detail::UboDetail, prog: u64, at: Recheck| {
            if s.address == 0 || s.size == 0 || s.size > SNAPSHOT_MAX_BYTES {
                return;
            }
            let slot_stage = s.slot | (s.stage as u32) << 24;
            let ix = ((s.address >> 4) ^ (s.address >> 16) ^ prog ^ u64::from(slot_stage)) as usize
                & 4095;
            let id = (s.address, s.size, slot_stage, prog);
            if seen[ix] == id {
                return;
            }
            seen[ix] = id;
            let rat = ((s.address >> 4) ^ (s.address >> 16) ^ u64::from(s.size)) as usize & 4095;
            if seen_range[rat] != (s.address, s.size) {
                seen_range[rat] = (s.address, s.size);
                ranges.push((s.address, s.size));
            }
            out.push(Binding {
                addr: s.address,
                size: s.size,
                key: Key {
                    prog,
                    slot_stage,
                    size: s.size.next_multiple_of(256),
                },
                at,
            });
        };
        // The hash of the last program key of each stage, by its allocation.
        let mut vs: Option<(usize, u64)> = None;
        let mut fs: Option<(usize, u64)> = None;
        // Draws that a template made are one allocation, and draws of a run bind the same buffers: a draw that binds what the
        // draw before it bound adds nothing.
        let mut prev_ptr = 0usize;
        let mut prev_sig = 0u64;
        for (di, d) in detail.draws.iter().enumerate() {
            if d.args.is_mesh() || d.sbufs.is_empty() {
                continue;
            }
            let ptr = Arc::as_ptr(d) as usize;
            if ptr == prev_ptr {
                continue;
            }
            prev_ptr = ptr;
            let mut sig = Arc::as_ptr(&d.program.vertex).cast::<u8>() as u64;
            sig = sig.rotate_left(7)
                ^ d.program
                    .fragment
                    .as_ref()
                    .map_or(0, |f| Arc::as_ptr(f).cast::<u8>() as u64);
            for s in &d.sbufs {
                sig = (sig.rotate_left(5) ^ s.address).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                    ^ u64::from(s.size)
                    ^ u64::from(s.slot | (s.stage as u32) << 24);
            }
            if sig == prev_sig {
                continue;
            }
            prev_sig = sig;
            for (si, s) in d.sbufs.iter().enumerate() {
                let (k, memo) = match s.stage {
                    Stage::Vertex => (Some(&d.program.vertex), &mut vs),
                    Stage::Fragment => (d.program.fragment.as_ref(), &mut fs),
                    _ => continue,
                };
                let Some(k) = k else { continue };
                let ptr = Arc::as_ptr(k).cast::<u8>() as usize;
                let prog = match memo {
                    Some((p, h)) if *p == ptr => *h,
                    _ => {
                        let h = self.prog_of(k);
                        *memo = Some((ptr, h));
                        h
                    }
                };
                push(s, prog, (0, di as u32, si as u32));
            }
        }
        for (di, d) in detail.dispatches.iter().enumerate() {
            let mut prog = None;
            for (si, s) in d.sbufs.iter().enumerate() {
                if s.stage != Stage::Compute {
                    continue;
                }
                let h = *prog.get_or_insert_with(|| {
                    foldhash::fast::FixedState::default().hash_one(d.program.as_str())
                });
                push(s, h, (1, di as u32, si as u32));
            }
        }
        ranges.sort_unstable();
        ranges.dedup();
        (out, ranges)
    }

    /// The bindings of a frame meet the ranges that the host knows. A range that is not known, bound by a key with a
    /// class, gets the class at once; a range that several keys bind gets what all of them need (a snapshot only reads,
    /// so it asks for nothing; a merge and a renamed range cannot be combined: `Conflict`, the frame waits in full).
    pub(super) fn adopt(&mut self, bindings: &[Binding]) -> Vec<Recheck> {
        let now = self.frame;
        let State {
            cands,
            keys,
            rw_known,
            ..
        } = self;
        // The bindings that the encoder has to look at: whether a shader writes them is not known yet.
        let mut recheck = Vec::new();
        for b in bindings {
            if let Some(c) = cands.get_mut(&b.addr) {
                let read_only = c.ro || c.learn.class() == Class::Snapshot;
                if b.size > c.size || (read_only && rw_known.get(&b.key) != Some(&false)) {
                    recheck.push(b.at);
                }
                if c.keys.len() < MAX_KEYS && !c.keys.contains(&b.key) {
                    c.keys.push(b.key);
                    if c.derived
                        && let Some(ki) = keys.get(&b.key)
                    {
                        combine(c, ki);
                    }
                }
            } else if cands.len() < MAX_CANDIDATES
                && let Some(ki) = keys.get(&b.key)
            {
                let mut learn = Learn::new();
                learn.set(ki.class);
                let mut c = Cand {
                    ro: ki.ro,
                    hits: SNAPSHOT_HITS,
                    cmask: ki.cmask.clone(),
                    gmask: ki.gmask.clone(),
                    grown: ki.grown,
                    // A range of a known class is not judged again soon, but a settled one is looked at again.
                    rest_until: if ki.class == Class::Never {
                        now
                    } else {
                        now + WATCH_PAUSE
                    },
                    size: b.size,
                    learn,
                    sample: None,
                    last: now,
                    // A settled key means a settled range: it is judged at the next judged flush, and for a long time
                    // only if it is quiet again.
                    quiet: if ki.class == Class::Never {
                        SETTLE_PROBES
                    } else {
                        0
                    },
                    clear_run: 0,
                    keys: vec![b.key],
                    derived: true,
                };
                if ki.class == Class::Merge && !probeable(b.addr, b.size) {
                    c.learn.set(Class::Never);
                }
                cands.insert(b.addr, c);
            } else {
                recheck.push(b.at);
            }
        }
        recheck
    }

    /// Tells the keys of the range at `addr` what was found for it.
    pub(super) fn push_keys(&mut self, addr: u64) {
        let State { cands, keys, .. } = self;
        let Some(c) = cands.get(&addr) else {
            return;
        };
        let class = c.learn.class();
        if !matches!(
            class,
            Class::Rename | Class::Snapshot | Class::Merge | Class::Conflict | Class::Never
        ) {
            return;
        }
        // `Never` is taught only once the range is settled: until then the CPU may still write it in some frames.
        if class == Class::Never && c.quiet < SETTLE_PROBES {
            return;
        }
        for k in &c.keys {
            match keys.get_mut(k) {
                None => {
                    keys.insert(
                        *k,
                        KeyInfo {
                            class,
                            ro: c.ro,
                            cmask: c.cmask.clone(),
                            gmask: c.gmask.clone(),
                            grown: c.grown,
                        },
                    );
                }
                // A range that is `Never` for its own reasons does not unteach a key.
                Some(_) if class == Class::Never => {}
                Some(ki) if ki.class == Class::Conflict => {}
                Some(ki) => {
                    ki.class = class;
                    ki.ro = c.ro;
                    union_mask(&mut ki.cmask, &c.cmask);
                    union_mask(&mut ki.gmask, &c.gmask);
                    ki.grown = ki.grown.max(c.grown);
                }
            }
        }
    }

    /// A candidate at `addr` is added: it is an event when the address was never a candidate.
    fn fresh(&mut self, addr: u64) {
        if self.seen.len() > 100_000 {
            self.seen.clear();
        }
        if self.seen.insert(addr) {
            self.last_event = self.frame;
        }
    }

    /// The flushes between two judged flushes now: every [`LEARN_PERIOD`] while something new is found, every
    /// [`SLOW_PERIOD`] after [`QUIET_FRAMES`] flushes without an event.
    pub(super) fn period(&self) -> u64 {
        if !self.fast_only && self.frame.saturating_sub(self.last_event) >= QUIET_FRAMES {
            SLOW_PERIOD
        } else {
            LEARN_PERIOD
        }
    }

    /// Adds a range that is only read. Its frames are judged for CPU writes in flight; it may become a snapshot.
    pub(super) fn watch_read_only(&mut self, addr: u64, size: u32) {
        if let Some(c) = self.cands.get_mut(&addr) {
            if c.ro && size > c.size {
                c.size = size;
                c.sample = None;
                c.hits = 0;
                c.learn = Learn::watch();
            }
        } else if self.cands.len() < MAX_CANDIDATES {
            self.fresh(addr);
            self.cands.insert(
                addr,
                Cand {
                    ro: true,
                    hits: 0,
                    cmask: Vec::new(),
                    gmask: Vec::new(),
                    grown: 0,
                    rest_until: 0,
                    size,
                    learn: Learn::watch(),
                    sample: None,
                    last: self.frame,
                    quiet: 0,
                    clear_run: 0,
                    keys: Vec::new(),
                    derived: false,
                },
            );
        }
    }

    /// Adds the range, which a shader may write, as a candidate, or grows it. A snapshot that is written turns into
    /// `Never` at once.
    pub(super) fn candidate(&mut self, addr: u64, size: u32) {
        if let Some(c) = self.cands.get_mut(&addr) {
            if c.learn.class() == Class::Snapshot {
                // A binding that is written is no snapshot, for any address it binds.
                for k in &c.keys {
                    if let Some(ki) = self.keys.get_mut(k) {
                        ki.class = Class::Never;
                    }
                }
                c.learn.set(Class::Never);
                c.ro = false;
                c.sample = None;
            } else if size > c.size || c.ro {
                c.size = size.max(c.size);
                c.ro = false;
                c.learn = Learn::new();
                c.sample = None;
            }
        } else if self.cands.len() < MAX_CANDIDATES {
            self.fresh(addr);
            self.cands.insert(
                addr,
                Cand {
                    ro: false,
                    hits: 0,
                    cmask: Vec::new(),
                    gmask: Vec::new(),
                    grown: 0,
                    rest_until: 0,
                    size,
                    learn: Learn::new(),
                    sample: None,
                    last: self.frame,
                    quiet: 0,
                    clear_run: 0,
                    keys: Vec::new(),
                    derived: false,
                },
            );
        }
    }

    /// A snapshot or merge range that the GPU or a copy writes in a way that the class cannot carry is state: `Never`.
    /// Returns whether it was one.
    pub(super) fn demote(&mut self, addr: u64) -> bool {
        match self.cands.get_mut(&addr) {
            Some(c) if matches!(c.learn.class(), Class::Snapshot | Class::Merge) => {
                c.learn.set(Class::Never);
                true
            }
            _ => false,
        }
    }

    /// True when the range has the class that `kind` needs.
    pub(super) fn has_kind(&self, addr: u64, kind: Kind) -> bool {
        let want = match kind {
            Kind::Rename => Class::Rename,
            Kind::Snapshot => Class::Snapshot,
            Kind::Merge => Class::Merge,
            // A probe is judged by the class that it measures; it has no class of its own.
            Kind::Probe => return true,
        };
        self.cands
            .get(&addr)
            .is_some_and(|c| c.learn.class() == want)
    }

    /// True when the range is a snapshot now.
    #[cfg(test)]
    pub(super) fn is_snapshot(&self, addr: u64) -> bool {
        self.has_kind(addr, Kind::Snapshot)
    }
}
