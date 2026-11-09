//! The templates of all handles, and the queue of memory writes that drops them.

use super::*;

/// The templates of one handle.
#[derive(Default)]
pub(super) struct Entry {
    /// Boxed, so that a hit takes a template out of the list and puts it back by moving a pointer.
    #[allow(clippy::vec_box)]
    pub(super) list: Vec<Box<Template>>,
    /// The keys that this handle has in `Cache::by_start`.
    keys: Vec<(u64, u64)>,
}

/// Counts why a call missed the templates of its handle (`NXKIT_DIAG=walkmiss`), and prints the most common
/// reasons every 20000 misses.
pub(super) fn walkmiss(why: String, templates: usize) {
    static MISSES: Mutex<Option<(u32, std::collections::HashMap<String, u32>)>> = Mutex::new(None);
    let mut g = MISSES.lock().unwrap_or_else(PoisonError::into_inner);
    let (n, map) = g.get_or_insert_with(Default::default);
    *n += 1;
    *map.entry(format!("{why} ({templates} kept)")).or_default() += 1;
    if *n % 20000 == 0 {
        let mut v: Vec<_> = map.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> = v
            .iter()
            .take(15)
            .map(|(k, c)| format!("{k}: {c}"))
            .collect();
        eprintln!("DIAG walkmiss after {n}: {}", top.join(" | "));
    }
}

/// The templates of all handles, and which memory each handle reads.
#[derive(Default)]
pub(in crate::walk) struct Cache {
    pub(super) entries: FastMap<u64, Entry>,
    /// `(start, handle)` to the end of each span of a recording that has templates.
    by_start: BTreeMap<(u64, u64), u64>,
    longest: u64,
    /// Handles that lost their templates because the game wrote their memory, since the last `take_counters`.
    pub(in crate::walk) invalidations: u32,
    /// Templates dropped because a handle had too many, since the last `take_counters`.
    pub(in crate::walk) evictions: u32,
    /// The evictions and hits of each handle since the last `clear`.
    pub(super) churn: FastMap<u64, (u32, u32)>,
    /// Handles that are called with more states in a frame than [`MAX_PER_HANDLE`]: a model recording that runs for
    /// each instance with another uniform buffer. Their templates are dropped as fast as they are made, so a call
    /// walks them without one. The walk still tracks into the template of the caller.
    pub(super) cold: FastMap<u64, ()>,
}

/// A handle with this many evictions turns cold when it had fewer than one hit per [`COLD_RATIO`] evictions.
pub(super) const COLD_AFTER: u32 = 4 * MAX_PER_HANDLE as u32;
pub(super) const COLD_RATIO: u32 = 8;

impl Cache {
    /// The number of templates that `handle` has.
    #[cfg(test)]
    pub(in crate::walk) fn templates(&self, handle: u64) -> usize {
        self.entries.get(&handle).map_or(0, |e| e.list.len())
    }

    pub(in crate::walk) fn clear(&mut self) {
        self.churn.clear();
        self.cold.clear();
        self.entries.clear();
        self.by_start.clear();
        self.longest = 0;
    }

    pub(super) fn insert(&mut self, handle: u64, t: Box<Template>) {
        if self.entries.len() >= MAX_HANDLES && !self.entries.contains_key(&handle) {
            self.clear();
        }
        let e = self.entries.entry(handle).or_default();
        if e.list.len() >= MAX_PER_HANDLE {
            e.list.remove(0);
            self.evictions += 1;
            let c = self.churn.entry(handle).or_default();
            c.0 += 1;
            if c.0 >= COLD_AFTER && c.1 * COLD_RATIO < c.0 {
                self.cold.insert(handle, ());
                if let Some(e) = self.entries.remove(&handle) {
                    for k in e.keys {
                        self.by_start.remove(&k);
                    }
                }
                return;
            }
        }
        for &(start, end) in &t.spans {
            match self.by_start.entry((start, handle)) {
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    if *o.get() < end {
                        o.insert(end);
                    }
                }
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(end);
                    e.keys.push((start, handle));
                }
            }
            self.longest = self.longest.max(end - start);
        }
        e.list.push(t);
    }

    pub(super) fn take(&mut self, handle: u64, i: usize) -> Option<Box<Template>> {
        let e = self.entries.get_mut(&handle)?;
        (i < e.list.len()).then(|| e.list.remove(i))
    }

    pub(super) fn put(&mut self, handle: u64, t: Box<Template>) {
        if let Some(e) = self.entries.get_mut(&handle) {
            e.list.push(t);
        }
    }

    pub(super) fn drop_handle(&mut self, handle: u64) {
        if let Some(e) = self.entries.remove(&handle) {
            for k in e.keys {
                self.by_start.remove(&k);
            }
            self.invalidations += 1;
        }
    }

    /// Drops the templates of every handle that read a record in `[start, end)`.
    pub(in crate::walk) fn invalidate(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let from = (start.saturating_sub(self.longest), 0);
        let hit: Vec<u64> = self
            .by_start
            .range(from..(end, 0))
            .filter(|(_, e)| **e > start)
            .map(|((_, h), _)| *h)
            .collect();
        for h in hit {
            self.drop_handle(h);
        }
    }
}

/// The ranges of guest memory that the game wrote, from threads that do not hold the walker.
///
/// The backend pushes a range when the front end learns about the write. The walker applies the ranges at the start of
/// its next [`Walker::submit`], before it reads any recording.
#[derive(Default)]
pub(crate) struct Invalidations {
    ranges: Mutex<Vec<(u64, u64)>>,
    dirty: AtomicBool,
}

impl Invalidations {
    /// Queues `[start, end)`.
    pub(crate) fn push(&self, start: u64, end: u64) {
        self.ranges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((start, end));
        self.dirty.store(true, Ordering::Release);
    }

    /// Moves the queued ranges to `out`.
    pub(super) fn drain(&self, out: &mut Vec<(u64, u64)>) {
        if self.dirty.swap(false, Ordering::Acquire) {
            out.append(&mut self.ranges.lock().unwrap_or_else(PoisonError::into_inner));
        }
    }
}
