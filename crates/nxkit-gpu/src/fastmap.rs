//! Maps and sets with the fast hasher of `foldhash`, for the whole crate.

use std::collections::{HashMap, HashSet};

/// A map with the fast hasher of `foldhash`. The keys of the per-draw maps are small integers and short keys that do
/// not need the protection of the default hasher. Build one with `FastMap::default()`.
pub(crate) type FastMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

/// A set with the fast hasher of `foldhash`. Build one with `FastSet::default()`.
pub(crate) type FastSet<K> = HashSet<K, foldhash::fast::RandomState>;
