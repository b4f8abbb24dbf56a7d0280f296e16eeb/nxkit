//! The state's flush: judging the samples of a frame and choosing the ranges it renames.

use super::*;

impl State {
    /// The flush of a frame. `bound` are its storage bindings, `others` is asked for only when a range is to be renamed,
    /// `read(addr, buf)` reads guest bytes.
    pub(super) fn flush(
        &mut self,
        fence: u64,
        bound: &[(u64, u32)],
        others: &BlockedBy,
        written: &dyn Fn() -> Vec<(u64, u64)>,
        read: &dyn Fn(u64, &mut [u8]) -> bool,
    ) -> Flushed {
        self.frame += 1;
        let mut out = Flushed::default();
        let t_judge = self.watch.then(std::time::Instant::now);
        for addr in std::mem::take(&mut self.pending_news) {
            if let Some(c) = self.cands.get(&addr) {
                out.news
                    .push((addr, c.size, c.learn.class(), c.learn.frames()));
            }
        }
        let mut touched: Vec<u64> = bound
            .iter()
            .filter(|(a, s)| self.cands.get(a).is_some_and(|c| *s <= c.size))
            .map(|(a, _)| *a)
            .collect();
        touched.sort_unstable();
        touched.dedup();
        let now = self.frame;
        let learn_frame = now.is_multiple_of(self.period());
        // A sample is judged at the flush right after its frame or never: its `post` and `pre_next` must be neighbours.
        for c in self.cands.values_mut() {
            if c.sample.as_ref().is_some_and(|s| s.frame + 1 != now) {
                c.sample = None;
            }
        }
        if now.is_multiple_of(60) {
            // The game allocates some of its storage ranges from rings, so addresses come and go: a range that is not
            // bound any more is forgotten, the sooner the less is known about it.
            self.cands.retain(|_, c| {
                let age = now - c.last;
                c.sample.is_some()
                    || match c.learn.class() {
                        Class::Learning | Class::Idle | Class::Watch => age <= 120,
                        Class::Never => age <= 600,
                        Class::Rename | Class::Snapshot | Class::Merge => age <= 3000,
                        Class::Conflict => age <= 600,
                    }
            });
        }
        let mut renaming: Vec<(u64, u32, Kind)> = Vec::new();
        let mut budget = WATCH_BUDGET;
        let mut copy_dsts: Option<Vec<(u64, u64)>> = None;
        for addr in touched {
            let Some(c) = self.cands.get_mut(&addr) else {
                continue;
            };
            let class = c.learn.class();
            if self.force != 0 {
                let bit = match class {
                    Class::Rename => 4,
                    Class::Merge => 2,
                    Class::Snapshot => 3,
                    Class::Conflict => 7,
                    _ if c.ro => 1,
                    _ => 0,
                };
                if bit < 5 && self.force >> bit & 1 != 0 {
                    out.forced |= 1 << bit;
                }
            }
            // A range that only waits to be judged (or has nothing to do) is looked at once in 60 flushes, to keep it from
            // being forgotten; the others are looked at in every flush.
            if matches!(class, Class::Never | Class::Watch)
                && !learn_frame
                && !now.is_multiple_of(60)
                && !(self.conflict_wait && !c.ro)
            {
                continue;
            }
            c.last = self.frame;
            c.learn.tick();
            if class == Class::Snapshot {
                // A copy or fill that writes the range makes it state.
                let dsts = copy_dsts.get_or_insert_with(written);
                let r = (addr, addr + u64::from(c.size));
                if dsts.iter().any(|&d| overlaps(r, d)) {
                    c.learn.set(Class::Never);
                    out.news
                        .push((addr, c.size, Class::Never, c.learn.frames()));
                } else {
                    renaming.push((addr, c.size, Kind::Snapshot));
                }
                continue;
            }
            if class == Class::Conflict
                || (self.conflict_wait
                    && !c.ro
                    && c.hits >= SNAPSHOT_HITS
                    && matches!(class, Class::Never | Class::Merge))
            {
                out.conflict = true;
            }
            // A range whose frames are judged for CPU writes in flight only: the Watch class and, to list the conflicts,
            // `Never` and `Idle` with `NXKIT_DIAG=rename`. The bytes that all of them read in a flush are limited.
            let in_flight = learn_frame
                && now >= c.rest_until
                && (matches!(class, Class::Watch | Class::Never | Class::Merge)
                    || (self.watch && class == Class::Idle))
                && (c.ro || probeable(addr, c.size) || class == Class::Idle)
                && u64::from(c.size) <= budget;
            let learn_judge = matches!(class, Class::Learning | Class::Rename);
            // Only a range that is judged in this flush is read; most bound ranges are not.
            let want = (c.sample.is_some() && learn_judge)
                || (learn_frame && c.learn.sampling())
                || in_flight;
            let mut pre = if want {
                vec![0u8; c.size as usize]
            } else {
                Vec::new()
            };
            let have = want && read(addr, &mut pre);
            if have {
                budget = budget.saturating_sub(u64::from(c.size));
            }
            if let Some(prev) = c.sample.take()
                && learn_judge
                && let Some(post) = prev.post
                && have
                && let Some(class) = c.learn.observe(judge(&prev.pre, &post, &pre))
            {
                out.news.push((addr, c.size, class, c.learn.frames()));
            }
            let judged_now = learn_frame && c.learn.sampling();
            if c.learn.class() == Class::Rename && !judged_now {
                renaming.push((addr, c.size, Kind::Rename));
            } else if c.learn.class() == Class::Merge && !in_flight {
                if probeable(addr, c.size) {
                    renaming.push((addr, c.size, Kind::Merge));
                }
            } else if (judged_now || in_flight) && have {
                c.sample = Some(Sample {
                    fence,
                    frame: now,
                    pre,
                    post: None,
                    atwait: None,
                });
                if !c.ro && matches!(c.learn.class(), Class::Never | Class::Merge) {
                    // A written range is judged on a copy (`Kind::Probe`); the flush says below whether it got one.
                    renaming.push((addr, c.size, Kind::Probe));
                } else if c.ro {
                    // A read-only range is measured when the wait begins; the frame needs no full wait.
                    out.watching = true;
                } else {
                    out.sampling = true;
                    out.checking |= c.learn.class() == Class::Rename;
                }
            }
        }
        if !out.news.is_empty() {
            self.last_event = now;
            let addrs: Vec<u64> = out.news.iter().map(|n| n.0).collect();
            for a in addrs {
                self.push_keys(a);
            }
        }
        if let Some(t) = t_judge {
            out.judge_ns = t.elapsed().as_nanos() as u64;
        }
        if renaming.is_empty() {
            return out;
        }
        let t_capture = self.watch.then(std::time::Instant::now);
        renaming.sort_unstable();
        let ranges: Vec<(u64, u32)> = renaming.iter().map(|&(a, s, _)| (a, s)).collect();
        // A range that passed the check against other uses in the last flushes is trusted for a while (the buffers that a
        // game binds do not move); the check is made for all others, and for all once in 60 flushes.
        let trusted: Vec<bool> = renaming
            .iter()
            .map(|&(a, _, k)| {
                k != Kind::Probe
                    && !now.is_multiple_of(60)
                    && self.cands.get(&a).is_some_and(|c| c.clear_run >= CLEAR_RUN)
            })
            .collect();
        let blocked = if trusted.iter().all(|t| *t) {
            vec![false; ranges.len()]
        } else {
            let mut b = others(&ranges);
            for (flag, t) in b.iter_mut().zip(&trusted) {
                *flag &= !t;
            }
            b
        };
        let (accepted, left) = plan(&ranges, bound, &blocked);
        out.excluded = left;
        for (i, &(addr, _, _)) in renaming.iter().enumerate() {
            if let Some(c) = self.cands.get_mut(&addr) {
                c.clear_run = if accepted.contains(&i) {
                    c.clear_run.saturating_add(1)
                } else {
                    0
                };
            }
        }
        let mut snapshot_budget = SNAPSHOT_BUDGET;
        let mut probed: Vec<u64> = Vec::new();
        for &i in &accepted {
            let (addr, size, kind) = renaming[i];
            if kind == Kind::Probe {
                probed.push(addr);
            }
            let snapshot = kind == Kind::Snapshot;
            if snapshot {
                if u64::from(size) > snapshot_budget {
                    out.over_budget += 1;
                    continue;
                }
                snapshot_budget -= u64::from(size);
            }
            if snapshot {
                out.snapshot_bytes += u64::from(size);
            }
            let words = (size as usize / 4).div_ceil(32);
            let mut x = Renamed {
                addr,
                len: size,
                start: Vec::new(),
                kind,
                cmask: Vec::new(),
                gmask: Vec::new(),
                rmask: Vec::new(),
                cvals: Vec::new(),
                cprefix: Vec::new(),
            };
            let ok = match kind {
                // A probe's copy is the guest range, filled on the GPU.
                Kind::Probe => {
                    x.cmask = vec![0; words];
                    x.cprefix = vec![0; words];
                    true
                }
                // A merge range needs only the words that the CPU writes in flight.
                Kind::Merge => match self.cands.get(&addr) {
                    Some(c) => {
                        let (g, cm) = (&c.gmask, &c.cmask);
                        let at = |m: &Vec<u32>, i: usize| m.get(i).copied().unwrap_or(0);
                        x.cmask = (0..words).map(|i| at(cm, i)).collect();
                        x.gmask = (0..words).map(|i| at(g, i) & !at(cm, i)).collect();
                        x.rmask = (0..words).map(|i| at(g, i) & at(cm, i)).collect();
                        match read_packed(read, addr, &x.cmask) {
                            Some((vals, prefix)) => {
                                x.cvals = vals;
                                x.cprefix = prefix;
                                true
                            }
                            None => false,
                        }
                    }
                    None => false,
                },
                _ => {
                    x.start = vec![0u8; size as usize];
                    read(addr, &mut x.start)
                }
            };
            if ok {
                out.renamed.push(x);
            } else {
                out.excluded += 1;
            }
        }
        // A probe that was not given a copy (another use of the range, the budget) judges nothing, and its frame needs
        // no full wait.
        for &(addr, _, kind) in &renaming {
            if kind == Kind::Probe
                && !probed.contains(&addr)
                && let Some(c) = self.cands.get_mut(&addr)
            {
                c.sample = None;
            }
        }
        out.sampling |= !probed.is_empty();
        out.renamed
            .retain(|x| x.kind != Kind::Probe || probed.contains(&x.addr));
        if let Some(t) = t_capture {
            out.capture_ns = t.elapsed().as_nanos() as u64;
        }
        out
    }
}
