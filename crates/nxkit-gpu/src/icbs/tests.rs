use std::sync::Arc;

use super::*;
use crate::draws::FastMap;
use crate::pools::{PagePins, PoolSet};

#[derive(Clone)]
struct Fake(u64);

impl Backing for Fake {
    fn real_base(&self) -> u64 {
        self.0
    }
}

struct NoPins;

impl PagePins for NoPins {
    fn pin(&self, _start: u64, _end: u64) {}
    fn unpin(&self, _start: u64, _end: u64) {}
}

const K16: u64 = 0x4000;

fn set() -> PoolSet<Fake> {
    PoolSet::new(
        0,
        0,
        |host: u64, _len: u64| Ok(Fake(host)),
        Arc::new(NoPins),
    )
}

/// The refs of an ICB that names the pool at `va`.
fn naming(set: &PoolSet<Fake>, va: u64) -> PoolRefs<Fake> {
    let snap = set.current();
    let mut refs = PoolRefs::new(&snap);
    let loc = snap.resolve(va, 1).unwrap();
    assert!(refs.take(&snap, loc).is_some());
    refs
}

#[test]
fn a_pool_is_noted_once() {
    let set = set();
    set.register(K16, K16, 0).unwrap();
    let snap = set.current();
    let mut refs = PoolRefs::new(&snap);
    for va in [K16, K16 + 8, K16 + 16] {
        refs.take(&snap, snap.resolve(va, 1).unwrap()).unwrap();
    }
    assert_eq!(refs.pools.len(), 1);
}

#[test]
fn the_buffer_lives_as_long_as_the_refs() {
    let set = set();
    let id = set.register(K16, K16, 0).unwrap();
    let refs = naming(&set, K16);
    let weak = Arc::downgrade(&refs.pools[0]._buf);
    set.unregister_id(id);
    assert!(weak.upgrade().is_some());
    drop(refs);
    assert!(weak.upgrade().is_none());
}

#[test]
fn nothing_changed_keeps_the_entry() {
    let set = set();
    set.register(K16, K16, 0).unwrap();
    let mut refs = naming(&set, K16);
    assert!(refs.current(&set.current()));
    // another pool comes: a new generation, the named pool is still there
    set.register(8 * K16, K16, 0).unwrap();
    assert!(refs.current(&set.current()));
    assert_eq!(refs.generation, set.current().generation());
}

#[test]
fn an_entry_whose_pool_id_vanished_is_rebuilt() {
    let set = set();
    let id = set.register(K16, K16, 0).unwrap();
    set.register(8 * K16, K16, 0).unwrap();
    let mut refs = naming(&set, K16);
    set.unregister_id(id);
    assert!(!refs.current(&set.current()));
}

#[test]
fn a_pool_registered_again_has_another_id() {
    let set = set();
    set.register(K16, K16, 0).unwrap();
    let mut refs = naming(&set, K16);
    assert!(set.unregister(K16));
    set.register(K16, K16, 0).unwrap();
    // the same range, but another registration
    assert!(!refs.current(&set.current()));
}

#[test]
fn forget_pools_drops_exactly_the_entries_that_name_a_pool() {
    let set = set();
    let a = set.register(K16, K16, 0).unwrap();
    let b = set.register(4 * K16, K16, 0).unwrap();
    let c = set.register(8 * K16, K16, 0).unwrap();
    let snap = set.current();
    let mut both = PoolRefs::new(&snap);
    both.take(&snap, snap.resolve(K16, 1).unwrap()).unwrap();
    both.take(&snap, snap.resolve(4 * K16, 1).unwrap()).unwrap();
    let mut map: FastMap<u64, PoolRefs<Fake>> = FastMap::default();
    map.insert(1, naming(&set, K16));
    map.insert(2, naming(&set, 4 * K16));
    map.insert(3, naming(&set, 8 * K16));
    map.insert(4, both);
    map.insert(5, PoolRefs::new(&snap));
    forget(&mut map, &[b], |r| r);
    let mut left: Vec<u64> = map.keys().copied().collect();
    left.sort();
    assert_eq!(left, vec![1, 3, 5]);
    forget(&mut map, &[a, c], |r| r);
    let mut left: Vec<u64> = map.keys().copied().collect();
    left.sort();
    assert_eq!(left, vec![5]);
    forget(&mut map, &[], |r| r);
    assert_eq!(map.len(), 1);
}
