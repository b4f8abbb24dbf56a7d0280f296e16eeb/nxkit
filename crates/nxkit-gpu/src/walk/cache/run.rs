//! The walk of a recording that goes through the cache, and its comparison with the uncached walk.

use super::*;

/// What a walk produced, for the comparison of a cached and an uncached walk.
pub(super) struct Produced {
    draws: Vec<Arc<DrawDetail>>,
    clears: Vec<ClearDetail>,
    copies: Vec<CopyDetail>,
    dispatches: Vec<DispatchDetail>,
    reports: Vec<CounterReport>,
    updated: Vec<(u64, u64)>,
    state: Box<State>,
}

impl Produced {
    /// The first difference with `other`, as text.
    fn diff(&self, other: &Produced) -> Option<String> {
        fn first<T: PartialEq + std::fmt::Debug>(name: &str, a: &[T], b: &[T]) -> Option<String> {
            if a.len() != b.len() {
                return Some(format!(
                    "{name}: {} entries cached, {} walked",
                    a.len(),
                    b.len()
                ));
            }
            let i = a.iter().zip(b).position(|(x, y)| x != y)?;
            Some(format!("{name}[{i}]: cached {:?}, walked {:?}", a[i], b[i]))
        }
        first("draws", &self.draws, &other.draws)
            .or_else(|| first("dispatches", &self.dispatches, &other.dispatches))
            .or_else(|| first("clears", &self.clears, &other.clears))
            .or_else(|| first("copies", &self.copies, &other.copies))
            .or_else(|| first("reports", &self.reports, &other.reports))
            .or_else(|| first("updated", &self.updated, &other.updated))
            .or_else(|| {
                (self.state != other.state).then(|| "the state after the walk differs".to_owned())
            })
    }
}

impl Walker {
    /// Applies the memory writes that were queued since the last call.
    pub(in crate::walk) fn drain_invalidations(&mut self) {
        let mut ranges = std::mem::take(&mut self.scratch_ranges);
        self.invalidations.drain(&mut ranges);
        for (start, end) in ranges.drain(..) {
            self.cache.invalidate(start, end);
        }
        self.scratch_ranges = ranges;
    }

    /// The queue of memory writes that the backend fills.
    pub(crate) fn invalidations(&self) -> Arc<Invalidations> {
        self.invalidations.clone()
    }

    pub(super) fn marks(&self) -> Marks {
        Marks {
            draws: self.out.draws.len(),
            clears: self.out.clears.len(),
            copies: self.out.copies.len(),
            dispatches: self.out.dispatches.len(),
            reports: self.out.reports.len(),
        }
    }

    /// Walks one recording that `submit` was given. With the verification on, it walks it twice.
    pub(in crate::walk) fn walk_top<M: GuestMem>(
        &mut self,
        mem: &mut M,
        st: &mut State,
        handle: u64,
    ) {
        if self.verify && self.cache_on {
            self.verify_top(mem, st, handle);
            return;
        }
        self.track.reset();
        self.run(mem, st, handle, 0);
    }

    fn produced(&self, marks: Marks, st: &State) -> Produced {
        Produced {
            draws: self.out.draws[marks.draws..].to_vec(),
            clears: self.out.clears[marks.clears..].to_vec(),
            copies: self.out.copies[marks.copies..].to_vec(),
            dispatches: self.out.dispatches[marks.dispatches..].to_vec(),
            reports: self.out.reports[marks.reports..].to_vec(),
            updated: self.updated.clone(),
            state: Box::new(st.clone()),
        }
    }

    /// Walks with the cache, takes the memory writes back, walks without it and compares. The uncached result stays.
    fn verify_top<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64) {
        let pre = st.clone();
        let updated = self.updated.clone();
        let snaps = self.snaps.get_mut().clone();
        let marks = self.marks();
        self.undo = Some(Vec::new());
        self.track.reset();
        self.run(mem, st, handle, 0);
        let undo = self.undo.take().unwrap_or_default();
        let cached = self.produced(marks, st);
        let counters = self.counters;
        for (host, old) in undo.iter().rev() {
            let _ = mem.write(*host, old);
        }
        self.updated = updated;
        *self.snaps.get_mut() = snaps;
        self.out.draws.truncate(marks.draws);
        self.out.clears.truncate(marks.clears);
        self.out.copies.truncate(marks.copies);
        self.out.dispatches.truncate(marks.dispatches);
        self.out.reports.truncate(marks.reports);
        *st = pre;
        self.bypass = true;
        self.track.reset();
        self.run(mem, st, handle, 0);
        self.bypass = false;
        self.counters = counters;
        let walked = self.produced(marks, st);
        if let Some(why) = cached.diff(&walked) {
            self.counters.verify_mismatches += 1;
            self.cache.clear();
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "nxkit-gpu: walker template mismatch at handle {handle:#x}, using the uncached walk: {why}"
                );
            }
            assert!(
                !under_test(),
                "walker template mismatch at handle {handle:#x}: {why}"
            );
        }
    }

    /// Walks the recording at `handle` that a call record names, from a template when one fits.
    pub(in crate::walk) fn run_nested<M: GuestMem>(
        &mut self,
        mem: &mut M,
        st: &mut State,
        handle: u64,
        depth: u32,
    ) {
        if !self.cache_on || self.bypass {
            self.walk_records(mem, st, handle, depth);
            return;
        }
        if let Some(i) = self.find(st, handle, depth)
            && let Some(mut t) = self.cache.take(handle, i)
        {
            self.counters.walk_hits += 1;
            self.counters.draws_from_template += t.draws.len() as u32;
            if let Some(c) = self.cache.churn.get_mut(&handle) {
                c.1 += 1;
            }
            if self.verify {
                self.play_verified(mem, st, handle, depth, &mut t);
            } else {
                self.play(mem, st, &mut t);
            }
            self.cache.put(handle, t);
            return;
        }
        let keep = !self.cache.cold.contains_key(&handle);
        if !keep {
            self.counters.cold_walks += 1;
        }
        self.build(mem, st, handle, depth, keep);
    }

    /// Plays the template, takes everything back, walks the recording without templates and compares. The uncached
    /// result stays. This finds the recording whose template is wrong, where [`Walker::verify_top`] only finds the
    /// top-level recording.
    fn play_verified<M: GuestMem>(
        &mut self,
        mem: &mut M,
        st: &mut State,
        handle: u64,
        depth: u32,
        t: &mut Template,
    ) {
        let pre = st.clone();
        let updated = self.updated.clone();
        let snaps = self.snaps.get_mut().clone();
        let marks = self.marks();
        let tracked = (
            self.track.dep_list.len(),
            self.track.patches.len(),
            self.track.upd_log.len(),
            self.track.tex_dep,
            self.track.prov,
        );
        let parent = self.track.open.last().map(|p| {
            (
                p.needs,
                p.written,
                p.programs.clone(),
                p.spans.clone(),
                p.height,
                p.truncated,
            )
        });
        let counters = self.counters;
        let outer = self.undo.replace(Vec::new());
        self.play(mem, st, t);
        let undo = self.undo.take().unwrap_or_default();
        let played = self.produced(marks, st);
        for (host, old) in undo.iter().rev() {
            let _ = mem.write(*host, old);
        }
        self.updated = updated;
        *self.snaps.get_mut() = snaps;
        self.out.draws.truncate(marks.draws);
        self.out.clears.truncate(marks.clears);
        self.out.copies.truncate(marks.copies);
        self.out.dispatches.truncate(marks.dispatches);
        self.out.reports.truncate(marks.reports);
        *st = pre;
        self.track.dep_list.truncate(tracked.0);
        self.track.patches.truncate(tracked.1);
        self.track.upd_log.truncate(tracked.2);
        self.track.tex_dep = tracked.3;
        self.track.prov = tracked.4;
        if let (Some(p), Some((needs, written, programs, spans, height, truncated))) =
            (self.track.open.last_mut(), parent)
        {
            (
                p.needs,
                p.written,
                p.programs,
                p.spans,
                p.height,
                p.truncated,
            ) = (needs, written, programs, spans, height, truncated);
        }
        self.undo = Some(Vec::new());
        self.bypass = true;
        self.walk_records(mem, st, handle, depth);
        self.bypass = false;
        let mut walk_undo = self.undo.take().unwrap_or_default();
        self.undo = outer;
        if let Some(outer) = &mut self.undo {
            outer.append(&mut walk_undo);
        }
        self.counters = counters;
        let walked = self.produced(marks, st);
        if let Some(why) = played.diff(&walked) {
            self.counters.verify_mismatches += 1;
            self.cache.clear();
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "nxkit-gpu: walker template mismatch for the recording {handle:#x} at depth {depth}: {why}"
                );
            }
            assert!(
                !under_test(),
                "walker template mismatch for the recording {handle:#x} at depth {depth}: {why}"
            );
        }
    }
}
