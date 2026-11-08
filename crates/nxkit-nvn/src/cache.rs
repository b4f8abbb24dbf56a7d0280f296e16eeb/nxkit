//! The cache of recording summaries.
//!
//! Games reuse recordings. Breath of the Wild records the commands of each model once and submits them in every frame.
//! The cache keeps the [`Summary`] of a recording by its handle, so a later submit does not decode again.
//!
//! A summary is valid for the inherited state that it needs. A handle can have several summaries for different states.
//! A summary is dropped when the game writes its command memory again, and so are the summaries of the recordings that call it.
//! A summary is also dropped when a texture address or a handle that it resolved now names something else.

use std::collections::BTreeMap;

/// A map with the fast hasher of `foldhash`. The keys are addresses and ids, so the map needs no protection against
/// crafted keys.
type HashMap<K, V> = std::collections::HashMap<K, V, foldhash::fast::RandomState>;

use crate::decode::{State, Summary};
use crate::descriptors::Descriptors;
use crate::resource::ResourceTable;

/// The most summaries that one handle keeps. The oldest goes first.
pub const MAX_ENTRIES_PER_HANDLE: usize = 64;
/// A summary is stable after this many hits.
pub const STABLE_HITS: u32 = 3;

/// The counters of the cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Lookups that found a valid summary.
    pub hits: u64,
    /// Lookups that decoded a recording.
    pub misses: u64,
    /// Handles that lost their summaries because the command memory changed.
    pub invalidations: u64,
    /// Summaries that were dropped because a texture or a handle changed.
    pub revalidations: u64,
    /// Summaries that were dropped because a handle had too many.
    pub evictions: u64,
    /// Addresses and handles that named no texture, counted at decode.
    pub unresolved: u64,
}

/// The summaries of one handle and the one that answered last.
#[derive(Debug, Default)]
struct Entries {
    list: Vec<Summary>,
    // a hint: a game submits the same recording under the same state again and again
    recent: usize,
    /// Summaries dropped because the list was full, and hits, since the handle was first seen.
    evicted: u32,
    hits: u32,
    /// A handle that is called with more states than [`MAX_ENTRIES_PER_HANDLE`] (a model recording that runs once per
    /// instance) drops its summaries as fast as it makes them. It keeps one summary, the last, and a lookup looks only
    /// at that one. It stays in `by_start` and `parents`, so a write to its memory still drops its callers.
    cold: bool,
}

/// A handle with this many evictions turns cold when it had fewer than one hit per [`COLD_RATIO`] evictions.
const COLD_AFTER: u32 = 64;
const COLD_RATIO: u32 = 8;

/// The summaries of the recordings of the game.
#[derive(Debug, Default)]
pub struct RecCache {
    entries: HashMap<u64, Entries>,
    // (start, handle) to end, for the recordings that occupy command memory
    by_start: BTreeMap<(u64, u64), u64>,
    parents: HashMap<u64, Vec<u64>>,
    longest: u64,
    /// The counters.
    pub stats: CacheStats,
    /// The counters and clocks of `NXKIT_DIAG=decodeprof`.
    pub prof: crate::prof::DecodeProf,
}

impl RecCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of handles that have a summary.
    pub fn handles(&self) -> usize {
        self.entries.len()
    }

    /// The summary at an index of a handle.
    pub fn entry(&self, handle: u64, index: usize) -> Option<&Summary> {
        self.entries.get(&handle)?.list.get(index)
    }

    /// Finds a summary of `handle` that fits the inherited state and still holds.
    pub fn find(
        &mut self,
        handle: u64,
        inherited: &State,
        table: &ResourceTable,
        desc: &Descriptors,
        mem: &impl crate::mem::GuestMem,
    ) -> Option<usize> {
        let e = self.entries.get_mut(&handle)?;
        let list = &mut e.list;
        let fits = |s: &Summary| s.key.matches(&s.needs, inherited);
        let i = match list.get(e.recent) {
            Some(s) if fits(s) => e.recent,
            _ if e.cold => return None,
            _ => {
                // The summaries of a handle almost always share their needs, so the hash of the inherited state is
                // made once per run of equal needs, and only a summary with the same hash is compared in full.
                let mut memo: Option<(crate::decode::Needs, u64)> = None;
                let mut found = None;
                for (i, s) in list.iter().enumerate() {
                    let h = match memo {
                        Some((n, h)) if n == s.needs => h,
                        _ => {
                            let h = crate::decode::Key::hash_of(&s.needs, inherited);
                            memo = Some((s.needs, h));
                            h
                        }
                    };
                    if h == s.key_hash && fits(s) {
                        found = Some(i);
                        break;
                    }
                }
                found?
            }
        };
        let clock = crate::prof::Clock::start();
        let ok = Self::still_holds(&mut list[i], table, desc, mem, &mut self.prof);
        self.prof.ns_refs += clock.ns();
        if !ok {
            self.prof.dropped += 1;
            list.remove(i);
            e.recent = 0;
            self.stats.revalidations += 1;
            return None;
        }
        e.recent = i;
        e.hits = e.hits.saturating_add(1);
        list[i].hits = list[i].hits.saturating_add(1);
        self.stats.hits += 1;
        Some(i)
    }

    /// True when every resolution of the summary is still the same. A texture address or a handle can only resolve to
    /// something else when a texture or a pool slot changed, and both count their changes, so a summary that was
    /// checked at the current counts is not walked again. A deferred bind reads guest memory, which changes without
    /// notice, so those are always read.
    fn still_holds(
        s: &mut Summary,
        table: &ResourceTable,
        desc: &Descriptors,
        mem: &impl crate::mem::GuestMem,
        prof: &mut crate::prof::DecodeProf,
    ) -> bool {
        let stamp = (table.epoch(), desc.epoch());
        if s.checked == stamp {
            prof.epoch_skips += 1;
        } else {
            prof.refs_checked += s.refs.len() as u64;
            let same = s.refs.iter().all(|r| {
                matches!(r, crate::decode::Ref::Deferred(..)) || r.holds(table, desc, mem)
            });
            if !same {
                return false;
            }
            s.checked = stamp;
        }
        s.deferred.iter().all(|r| r.holds(table, desc, mem))
    }

    /// Keeps a summary and returns its index. A new recording drops the summaries of the memory that it overwrote.
    pub fn insert(&mut self, handle: u64, s: Summary) -> usize {
        let fresh = !self.entries.contains_key(&handle);
        if fresh {
            for r in &s.spans {
                self.longest = self.longest.max(r.end - r.start);
                self.by_start.insert((r.start, handle), r.end);
            }
        }
        for c in &s.children {
            let p = self.parents.entry(*c).or_default();
            if !p.contains(&handle) {
                p.push(handle);
            }
        }
        let e = self.entries.entry(handle).or_default();
        if e.cold {
            e.list.clear();
        } else if e.list.len() >= MAX_ENTRIES_PER_HANDLE {
            e.list.remove(0);
            self.stats.evictions += 1;
            e.evicted += 1;
            if e.evicted >= COLD_AFTER && e.hits.saturating_mul(COLD_RATIO) < e.evicted {
                e.cold = true;
                e.list.clear();
            }
        }
        e.list.push(s);
        e.recent = e.list.len() - 1;
        e.list.len() - 1
    }

    /// Tells that the game wrote `[start, end)` of host memory.
    /// Every handle whose records overlap loses its summaries. The recordings that call it lose theirs too.
    pub fn invalidate(&mut self, start: u64, end: u64) {
        let from = (start.saturating_sub(self.longest), 0);
        let hit: Vec<u64> = self
            .by_start
            .range(from..(end, 0))
            .filter(|((_, _), e)| **e > start)
            .map(|((_, h), _)| *h)
            .collect();
        for h in hit {
            self.drop_handle(h);
        }
    }

    fn drop_handle(&mut self, handle: u64) {
        let Some(Entries { list, .. }) = self.entries.remove(&handle) else {
            return;
        };
        self.stats.invalidations += 1;
        for s in &list {
            for r in &s.spans {
                self.by_start.remove(&(r.start, handle));
            }
            for c in &s.children {
                if let Some(p) = self.parents.get_mut(c) {
                    p.retain(|x| *x != handle);
                }
            }
        }
        for p in self.parents.remove(&handle).unwrap_or_default() {
            self.drop_handle(p);
        }
    }

    /// The summaries that could become indirect command buffers and are stable: no blocking command and enough hits.
    /// The result is the handle, the number of draws and the number of hits. The limits of Metal are not checked here.
    pub fn icb_candidates(&self) -> Vec<(u64, u32, u32)> {
        let mut out: Vec<_> = self
            .entries
            .iter()
            .flat_map(|(h, l)| l.list.iter().map(move |s| (*h, s)))
            .filter(|(_, s)| s.blockers == 0 && s.draws > 0 && s.hits >= STABLE_HITS)
            .map(|(h, s)| (h, s.draws, s.hits))
            .collect();
        out.sort_unstable();
        out
    }
}
