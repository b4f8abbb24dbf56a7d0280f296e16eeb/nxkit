//! A small least-recently-used map for a handful of entries (the open archive shards). The entries are in a vector, the
//! most recently used last: a few dozen entries do not need more.

/// At most `cap` entries. Inserting into a full map drops the least recently used one.
pub(crate) struct Lru<K, V> {
    cap: usize,
    items: Vec<(K, V)>,
}

impl<K: PartialEq, V> Lru<K, V> {
    /// An empty map of room for `cap` entries (at least one).
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            items: Vec::new(),
        }
    }

    /// The entry under `key`, now the most recently used.
    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        let at = self.items.iter().position(|(k, _)| k == key)?;
        let e = self.items.remove(at);
        self.items.push(e);
        self.items.last().map(|(_, v)| v)
    }

    /// Adds `key` as the most recently used and returns the entries it pushed out (oldest first). An entry under the
    /// same key is replaced and returned as well.
    pub(crate) fn insert(&mut self, key: K, value: V) -> Vec<(K, V)> {
        let mut out = Vec::new();
        if let Some(at) = self.items.iter().position(|(k, _)| *k == key) {
            out.push(self.items.remove(at));
        }
        self.items.push((key, value));
        while self.items.len() > self.cap {
            out.push(self.items.remove(0));
        }
        out
    }

    /// The number of entries.
    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    /// The keys, least recently used first.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<&K> {
        self.items.iter().map(|(k, _)| k).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_recently_used_entry_goes_first() {
        let mut l = Lru::new(2);
        assert!(l.insert(1, "a").is_empty());
        assert!(l.insert(2, "b").is_empty());
        assert_eq!(l.get(&1), Some(&"a"));
        // 2 is now the oldest
        assert_eq!(l.insert(3, "c"), vec![(2, "b")]);
        assert_eq!(l.keys(), vec![&1, &3]);
        assert_eq!(l.get(&2), None);
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn a_miss_does_not_touch_the_order() {
        let mut l = Lru::new(3);
        l.insert(1, ());
        l.insert(2, ());
        assert!(l.get(&9).is_none());
        assert_eq!(l.keys(), vec![&1, &2]);
    }

    #[test]
    fn the_same_key_is_replaced() {
        let mut l = Lru::new(2);
        l.insert(1, "a");
        l.insert(2, "b");
        assert_eq!(l.insert(1, "c"), vec![(1, "a")]);
        assert_eq!(l.len(), 2);
        assert_eq!(l.get(&1), Some(&"c"));
    }

    #[test]
    fn room_for_none_still_holds_one() {
        let mut l = Lru::new(0);
        l.insert(1, ());
        assert_eq!(l.insert(2, ()), vec![(1, ())]);
        assert_eq!(l.len(), 1);
    }
}
