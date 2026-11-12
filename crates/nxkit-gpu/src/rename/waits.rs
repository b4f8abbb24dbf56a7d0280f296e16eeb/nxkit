//! The state's side of the waits: the bytes after a frame, the classes and the probes.

use super::*;

impl State {
    /// The CPU waited and the GPU is done with the frames up to `done`; `newest` is the newest flushed fence. Reads the
    /// bytes after the frame of each sample that no later flush has overtaken.
    pub(super) fn waited(&mut self, done: u64, newest: u64, read: &dyn Fn(u64, &mut [u8]) -> bool) {
        let now = self.frame;
        let mut found: Vec<(u64, Inflight, usize)> = Vec::new();
        let mut news: Vec<u64> = Vec::new();
        for (addr, c) in &mut self.cands {
            // A written range that is judged on a copy is measured in `probed`.
            if c.ro || matches!(c.learn.class(), Class::Never | Class::Merge) {
                continue;
            }
            if let Some(s) = &mut c.sample
                && s.post.is_none()
                && s.fence == newest
                && done >= s.fence
            {
                let mut post = vec![0u8; c.size as usize];
                if read(*addr, &mut post) {
                    if let Some(aw) = &s.atwait {
                        let cm = cpu_mask(&s.pre, aw);
                        let n = cm.iter().map(|w| w.count_ones() as usize).sum::<usize>();
                        found.push((
                            *addr,
                            Inflight {
                                size: c.size,
                                class: c.learn.class().name(),
                                ro: c.ro,
                                ..Inflight::default()
                            },
                            n,
                        ));
                        if n > 0 {
                            c.hits += 1;
                            // A range that shaders only read and the CPU writes while frames run is a snapshot.
                            if c.ro && c.learn.class() == Class::Watch && c.hits >= SNAPSHOT_HITS {
                                c.learn.set(Class::Snapshot);
                                news.push(*addr);
                            }
                        } else {
                            c.rest_until = now + WATCH_PAUSE;
                        }
                    }
                    s.post = Some(post);
                }
            }
        }
        self.pending_news.extend(news);
        self.classify();
        for (addr, f, n) in found {
            let e = self.inflight.entry(addr).or_default();
            e.size = f.size;
            e.class = f.class;
            e.ro = f.ro;
            e.frames += 1;
            if n > 0 {
                e.hit += 1;
                e.words = e.words.max(n);
            }
        }
    }

    /// A written range that the CPU writes in flight is a merge range (one class for all of them): the words that only
    /// the GPU writes (`P`) are merged on the GPU, the words that both write (`R`) are written back by the host in the
    /// wait. A merge range whose masks keep growing after it was classified is not understood: `Conflict`, the wait for
    /// its frame is a full one. What cannot be seen: an `R` word that the CPU writes in flight after the GPU wrote it in
    /// the same frame looks like one written before; its GPU value is dropped, as for a word that the CPU re-initialises.
    fn classify(&mut self) {
        let now = self.frame;
        let mut news: Vec<u64> = Vec::new();
        for (addr, c) in &mut self.cands {
            if c.ro
                || c.hits < SNAPSHOT_HITS
                || !matches!(c.learn.class(), Class::Never | Class::Merge)
            {
                continue;
            }
            if c.learn.class() == Class::Merge {
                if c.grown > MAX_GROWN {
                    c.learn.set(Class::Conflict);
                    note_n(Reason::OverlapMergeConflict, 1);
                    news.push(*addr);
                }
            } else if probeable(*addr, c.size) {
                c.learn.set(Class::Merge);
                c.grown = 0;
                c.rest_until = now + WATCH_PAUSE;
                news.push(*addr);
            }
        }
        self.pending_news.extend(news);
    }

    /// The GPU is done with the judged frame `fence`, in which the written range `addr` read and wrote a copy: `end` is the
    /// copy after the frame and `start` the copy as the kernel filled it. The words that the GPU wrote are those where the
    /// two differ. The words that the CPU wrote are those where guest memory at the start of the wait differs from `pre`,
    /// except where it holds what the GPU wrote (the frame can end before the wait begins and its words go back into guest
    /// memory).
    pub(super) fn probed(&mut self, fence: u64, addr: u64, end: &[u8], start: &[u8]) {
        let now = self.frame;
        let Some(c) = self.cands.get_mut(&addr) else {
            return;
        };
        let Some(s) = c.sample.as_ref().filter(|s| s.fence == fence) else {
            return;
        };
        let Some(aw) = &s.atwait else {
            return;
        };
        let gm = gpu_words(end, start);
        let mut cm = cpu_mask(&s.pre, aw);
        for (i, (a, e)) in aw.chunks(4).zip(end.chunks(4)).enumerate() {
            if gm[i / 32] >> (i % 32) & 1 != 0 && a == e {
                cm[i / 32] &= !(1 << (i % 32));
            }
        }
        let n = cm.iter().map(|w| w.count_ones() as usize).sum::<usize>();
        let grew = union_mask(&mut c.cmask, &cm) | union_mask(&mut c.gmask, &gm);
        if grew && c.learn.class() == Class::Merge {
            c.grown += 1;
        }
        if c.learn.class() == Class::Merge {
            // A merge range is looked at again after a while.
            c.rest_until = now + WATCH_PAUSE;
        }
        if n > 0 {
            c.hits += 1;
            c.quiet = 0;
        } else {
            // A range that the CPU writes only in some frames (while the player moves) is not given up for one quiet
            // frame; after [`SETTLE_PROBES`] it is settled and looked at again after [`REPROBE`] flushes.
            c.quiet += 1;
            if c.learn.class() != Class::Merge {
                c.rest_until = if c.quiet >= SETTLE_PROBES {
                    now + REPROBE
                } else {
                    now
                };
            }
        }
        let e = self.inflight.entry(addr).or_default();
        e.size = c.size;
        e.class = c.learn.class().name();
        e.ro = false;
        e.frames += 1;
        if n > 0 {
            e.hit += 1;
            e.words = e.words.max(n);
        }
        self.classify();
        self.push_keys(addr);
    }

    /// The wait for `fence` begins: takes the bytes of the ranges that are judged in that frame before the GPU is known to
    /// be done. A read-only range is measured now: the words that changed since the flush are the CPU's writes in flight,
    /// and the GPU cannot have written them.
    pub(super) fn before_wait(&mut self, fence: u64, read: &dyn Fn(u64, &mut [u8]) -> bool) {
        let now = self.frame;
        let mut seen: Vec<(u64, usize)> = Vec::new();
        for (addr, c) in &mut self.cands {
            if let Some(s) = &mut c.sample
                && s.fence == fence
                && s.atwait.is_none()
            {
                let mut b = vec![0u8; c.size as usize];
                if read(*addr, &mut b) {
                    if c.ro {
                        let n = cpu_mask(&s.pre, &b)
                            .iter()
                            .map(|w| w.count_ones() as usize)
                            .sum();
                        seen.push((*addr, n));
                    }
                    s.atwait = Some(b);
                }
            }
        }
        let mut news = Vec::new();
        for (addr, n) in seen {
            let Some(c) = self.cands.get_mut(&addr) else {
                continue;
            };
            if n > 0 {
                c.hits += 1;
                c.quiet = 0;
                // A range that shaders only read and the CPU writes while frames run is a snapshot.
                if c.learn.class() == Class::Watch && c.hits >= SNAPSHOT_HITS {
                    c.learn.set(Class::Snapshot);
                    news.push(addr);
                }
            } else {
                c.quiet += 1;
                c.rest_until = if c.quiet >= SETTLE_PROBES {
                    now + REPROBE
                } else {
                    now
                };
            }
            c.sample = None;
            let e = self.inflight.entry(addr).or_default();
            e.size = c.size;
            e.class = c.learn.class().name();
            e.ro = true;
            e.frames += 1;
            if n > 0 {
                e.hit += 1;
                e.words = e.words.max(n);
            }
        }
        self.pending_news.extend(news);
    }

    /// The classes of the candidates: learning, rename, never, idle.
    pub(super) fn classes(&self) -> [usize; 8] {
        let mut n = [0; 8];
        for c in self.cands.values() {
            n[c.learn.class() as usize] += 1;
        }
        n
    }

    pub(super) fn waiting(&self) -> bool {
        self.cands
            .values()
            .any(|c| c.sample.as_ref().is_some_and(|s| s.post.is_none()))
    }
}
