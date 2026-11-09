//! A small least-recently-used map. The caches that use it hold a handful of entries, so the oldest entry is found
//! by a scan instead of a linked list.

use std::collections::HashMap;
use std::hash::Hash;

pub(crate) struct Lru<K, V> {
    cap: usize,
    tick: u64,
    map: HashMap<K, (V, u64)>,
}

impl<K: Hash + Eq + Clone, V> Lru<K, V> {
    /// A map that keeps at most `cap` entries (at least one).
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            tick: 0,
            map: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// The entry for `key`; it counts as the most recently used.
    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(key).map(|e| {
            e.1 = tick;
            &e.0
        })
    }

    /// Stores `value`. When the map is full, the least recently used entry goes first. Returns the entry that left.
    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.tick += 1;
        let mut out = None;
        if !self.map.contains_key(&key) && self.map.len() >= self.cap {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.1)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                out = self.map.remove(&k).map(|e| e.0);
            }
        }
        self.map.insert(key, (value, self.tick));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_the_least_recently_used() {
        let mut l = Lru::new(2);
        l.insert(1, "a");
        l.insert(2, "b");
        assert_eq!(l.get(&1), Some(&"a"));
        assert_eq!(l.insert(3, "c"), Some("b"));
        assert!(l.contains(&1) && l.contains(&3) && !l.contains(&2));
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn replacing_a_key_does_not_evict() {
        let mut l = Lru::new(2);
        l.insert(1, "a");
        l.insert(2, "b");
        assert_eq!(l.insert(1, "c"), None);
        assert_eq!(l.len(), 2);
        assert_eq!(l.get(&1), Some(&"c"));
    }

    #[test]
    fn capacity_is_at_least_one() {
        let mut l = Lru::new(0);
        l.insert(1, ());
        l.insert(2, ());
        assert_eq!(l.len(), 1);
        assert!(l.contains(&2));
    }
}
