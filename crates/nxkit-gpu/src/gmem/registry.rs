//! The registrations of the memory pools of `gmem`.

use std::collections::BTreeMap;

/// The registrations of the memory pools: every pool until it is removed. Pools may overlap and may share a start.
#[derive(Debug, Default)]
pub(super) struct Registry {
    /// `start -> ends` of the pools with that start, in guest GPU addresses, in the order they were registered.
    by_start: BTreeMap<u64, Vec<u64>>,
}

impl Registry {
    /// Adds one registration. An empty pool is not kept. Returns whether one was added.
    pub(super) fn add(&mut self, start: u64, len: u64) -> bool {
        if len == 0 {
            return false;
        }
        self.by_start
            .entry(start)
            .or_default()
            .push(start.saturating_add(len));
        true
    }

    /// Removes the oldest registration with this start, the one whose removal arrives. Returns whether there was one.
    pub(super) fn remove(&mut self, start: u64) -> bool {
        let Some(ends) = self.by_start.get_mut(&start) else {
            return false;
        };
        ends.remove(0);
        if ends.is_empty() {
            self.by_start.remove(&start);
        }
        true
    }

    /// The number of registrations.
    fn len(&self) -> usize {
        self.by_start.values().map(Vec::len).sum()
    }

    /// Every registration `(start, end)`, sorted by start; for equal starts the longer one comes first.
    pub(super) fn sorted(&self) -> Vec<(u64, u64)> {
        let mut out = Vec::with_capacity(self.len());
        for (s, ends) in &self.by_start {
            let first = out.len();
            out.extend(ends.iter().map(|e| (*s, *e)));
            out[first..].sort_unstable_by_key(|a| std::cmp::Reverse(a.1));
        }
        out
    }
}

/// The number of registrations in `sorted` (see `Registry::sorted`) that overlap an earlier one.
pub(super) fn count_overlapping(sorted: &[(u64, u64)]) -> u32 {
    let (mut reach, mut n) = (0u64, 0u32);
    for &(start, end) in sorted {
        if start < reach {
            n += 1;
        }
        reach = reach.max(end);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gmem::{ENTRY_BYTES, Table, build_table};

    fn u64_at(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    /// The table of the registrations, as `refresh` builds it with a delta of the start plus 1.
    fn table_of(r: &Registry) -> Table {
        build_table(r.sorted().into_iter().map(|(s, e)| (s, e, s + 1)))
    }

    /// The delta of the entry that holds `addr`, read from the bytes of the table the way the shader searches them.
    fn lookup(t: &Table, addr: u64) -> Option<u64> {
        (1..t.ranges.len()).find_map(|k| {
            let lo = u64_at(&t.bytes, k * ENTRY_BYTES);
            let hi = u64_at(&t.bytes, k * ENTRY_BYTES + 8);
            (lo <= addr && addr < hi).then(|| u64_at(&t.bytes, k * ENTRY_BYTES + 16))
        })
    }

    #[test]
    fn a_pool_inside_another_does_not_remove_it() {
        let mut r = Registry::default();
        r.add(0x1000, 0x4000);
        r.add(0x2000, 0x800);
        let t = table_of(&r);
        assert_eq!(t.ranges, vec![(0, 0), (0x1000, 0x5000)]);
        for a in (0x1000..0x5000).step_by(0x100) {
            assert!(lookup(&t, a).is_some(), "{a:#x}");
        }
        assert!(r.remove(0x2000));
        let t = table_of(&r);
        for a in (0x1000..0x5000).step_by(0x100) {
            assert!(lookup(&t, a).is_some(), "{a:#x}");
        }
        assert_eq!(lookup(&t, 0x5000), None);
        assert_eq!(count_overlapping(&[(0x1000, 0x5000), (0x2000, 0x2800)]), 1);
    }

    #[test]
    fn a_new_pool_at_the_start_of_an_old_one_survives_the_removal_of_the_old_one() {
        let mut r = Registry::default();
        r.add(0x1000, 0x1000);
        r.add(0x1000, 0x3000);
        // the table covers the union while both live
        assert_eq!(table_of(&r).ranges, vec![(0, 0), (0x1000, 0x4000)]);
        assert!(r.remove(0x1000));
        let t = table_of(&r);
        assert_eq!(t.ranges, vec![(0, 0), (0x1000, 0x4000)]);
        assert!(lookup(&t, 0x3800).is_some());
    }

    #[test]
    fn overlapping_pools_cover_their_union() {
        let mut r = Registry::default();
        r.add(0x1000, 0x2000);
        r.add(0x2000, 0x2000);
        let t = table_of(&r);
        assert!(lookup(&t, 0x1800).is_some());
        assert!(lookup(&t, 0x2800).is_some());
        assert!(lookup(&t, 0x3800).is_some());
        assert_eq!(lookup(&t, 0x4000), None);
        assert_eq!(lookup(&t, 0xfff), None);
        for w in t.ranges[1..].windows(2) {
            assert!(w[0].1 <= w[1].0 && w[0].0 < w[0].1);
        }
        assert_eq!(t.over, 0);
        assert_eq!(count_overlapping(&r.sorted()), 1);
    }

    #[test]
    fn removing_the_last_registration_of_a_start_removes_the_range() {
        let mut r = Registry::default();
        r.add(0x1000, 0x1000);
        r.add(0x8000, 0x1000);
        assert!(r.remove(0x1000));
        assert!(!r.remove(0x1000));
        assert_eq!(table_of(&r).ranges, vec![(0, 0), (0x8000, 0x9000)]);
        assert_eq!(r.len(), 1);
        assert!(!r.add(0x4000, 0));
    }

    #[test]
    fn the_oldest_registration_of_a_start_goes_first() {
        let mut r = Registry::default();
        r.add(0x1000, 0x1000);
        r.add(0x1000, 0x3000);
        r.remove(0x1000);
        assert_eq!(r.sorted(), vec![(0x1000, 0x4000)]);
        // two pools with the same start, the longer first: the shorter adds no range
        let t = build_table([(0x1000, 0x4000, 1), (0x1000, 0x2000, 2)]);
        assert_eq!(t.ranges, vec![(0, 0), (0x1000, 0x4000)]);
        assert_eq!(t.over, 0);
    }
}
