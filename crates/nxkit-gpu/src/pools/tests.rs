use std::sync::{Arc, Mutex};

use super::*;

const VA: u64 = 0xC0_0000_0000;
const HOST: u64 = 0x10_0000_0000;
const K4: u64 = 0x1000;
const K16: u64 = 0x4000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ev {
    Pin(u64, u64),
    Make(u64, u64),
    Drop(u64),
    Unpin(u64, u64),
}

type Log = Arc<Mutex<Vec<Ev>>>;

struct Fake {
    start: u64,
    log: Log,
}

impl Backing for Fake {
    fn real_base(&self) -> u64 {
        0x1000_0000 + self.start
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.log.lock().unwrap().push(Ev::Drop(self.start));
    }
}

struct Pins(Log);

impl PagePins for Pins {
    fn pin(&self, start: u64, end: u64) {
        self.0.lock().unwrap().push(Ev::Pin(start, end));
    }
    fn unpin(&self, start: u64, end: u64) {
        self.0.lock().unwrap().push(Ev::Unpin(start, end));
    }
}

fn events(log: &Log) -> Vec<Ev> {
    std::mem::take(&mut *log.lock().unwrap())
}

fn set_with(fail: bool) -> (PoolSet<Fake>, Log) {
    let log: Log = Arc::default();
    let l = log.clone();
    let maker = move |start: u64, len: u64| {
        if fail {
            return Err("refused".to_string());
        }
        l.lock().unwrap().push(Ev::Make(start, len));
        Ok(Fake {
            start,
            log: l.clone(),
        })
    };
    let set = PoolSet::new(VA, HOST, maker, Arc::new(Pins(log.clone())));
    (set, log)
}

fn set() -> (PoolSet<Fake>, Log) {
    set_with(false)
}

/// The host address of a guest address.
fn host(va: u64) -> u64 {
    va - VA + HOST
}

#[test]
fn resolve_inside_and_edges() {
    let (set, _) = set();
    set.register(VA + 0x10000, 0x8000, 0).unwrap();
    let snap = set.current();
    let loc = snap.resolve(VA + 0x10100, 16).unwrap();
    assert_eq!(loc.pool, 0);
    assert_eq!(loc.offset, 0x100);
    assert_eq!(loc.real, 0x1000_0000 + host(VA + 0x10000) + 0x100);
    assert_eq!(loc.room, 0x8000 - 0x100);
    // First byte and last byte.
    assert!(snap.resolve(VA + 0x10000, 1).is_ok());
    assert!(snap.resolve(VA + 0x17fff, 1).is_ok());
    // The whole pool.
    assert!(snap.resolve(VA + 0x10000, 0x8000).is_ok());
    // One byte too many.
    assert_eq!(
        snap.resolve(VA + 0x10000, 0x8001),
        Err(Miss::CrossesEdge {
            pool_end: VA + 0x18000
        })
    );
}

#[test]
fn resolve_misses() {
    let (set, _) = set();
    set.register(VA + 0x10000, 0x8000, 0).unwrap();
    let snap = set.current();
    assert_eq!(snap.resolve(VA + 0xffff, 1), Err(Miss::NoPool));
    // The end of a pool is not in it.
    assert_eq!(snap.resolve(VA + 0x18000, 1), Err(Miss::NoPool));
    assert_eq!(snap.resolve(VA + 0x18000, 0), Err(Miss::NoPool));
    assert_eq!(snap.resolve(0, 4), Err(Miss::NoPool));
    // A range that crosses the edge.
    assert_eq!(
        snap.resolve(VA + 0x17ffc, 8),
        Err(Miss::CrossesEdge {
            pool_end: VA + 0x18000
        })
    );
    // Overflow of va + len is a miss.
    assert!(matches!(
        snap.resolve(VA + 0x10000, u64::MAX),
        Err(Miss::CrossesEdge { .. })
    ));
    assert_eq!(snap.resolve(u64::MAX, 2), Err(Miss::NoPool));
    // Empty snapshot.
    assert_eq!(
        PoolSnapshot::<Fake>::empty().resolve(VA, 1),
        Err(Miss::NoPool)
    );
}

#[test]
fn zero_length_resolves_inside_a_pool() {
    let (set, _) = set();
    set.register(VA + 0x10000, 0x8000, 0).unwrap();
    let snap = set.current();
    assert!(snap.resolve(VA + 0x10000, 0).is_ok());
    assert!(snap.resolve(VA + 0x17fff, 0).is_ok());
    assert_eq!(snap.resolve(VA + 0x8000, 0), Err(Miss::NoPool));
}

#[test]
fn resolve_start_gives_room() {
    let (set, _) = set();
    set.register(VA + 0x10000, 0x8000, 0).unwrap();
    set.register(VA + 0x20000, 0x4000, 0).unwrap();
    let snap = set.current();
    let a = snap.resolve_start(VA + 0x17000).unwrap();
    assert_eq!((a.pool, a.room), (0, 0x1000));
    let b = snap.resolve_start(VA + 0x20000).unwrap();
    assert_eq!((b.pool, b.offset, b.room), (1, 0, 0x4000));
    // The length is not looked at.
    assert!(snap.resolve_start(VA + 0x17fff).is_ok());
    assert_eq!(snap.resolve_start(VA + 0x18000), Err(Miss::NoPool));
}

#[test]
fn a_new_pool_replaces_the_ones_it_overlaps() {
    let (set, _) = set();
    let a = set.register(VA + 0x10000, 0x4000, 0).unwrap();
    let b = set.register(VA + 0x14000, 0x4000, 0).unwrap();
    let c = set.register(VA + 0x18000, 0x4000, 0).unwrap();
    let d = set.register(VA + 0x30000, 0x4000, 0).unwrap();
    // Covers the second and the third pool, and half of the first.
    let n = set.register(VA + 0x12000, 0x8000, 0).unwrap();
    let snap = set.current();
    let ids: Vec<u64> = snap.entries().iter().map(|e| e.id).collect();
    // The new pool overlaps the second half of the first pool and the whole of the next two: all three go.
    assert_eq!(ids, vec![n, d]);
    let _ = (a, b, c);
}

#[test]
fn a_new_pool_keeps_the_neighbours_it_does_not_overlap() {
    let (set, _) = set();
    let a = set.register(VA + 0x10000, 0x4000, 0).unwrap();
    let b = set.register(VA + 0x18000, 0x4000, 0).unwrap();
    let n = set.register(VA + 0x14000, 0x4000, 0).unwrap();
    let snap = set.current();
    let ids: Vec<u64> = snap.entries().iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![a, n, b]);
    // Same address again: the new entry has a new id.
    let again = set.register(VA + 0x14000, 0x4000, 0).unwrap();
    assert_ne!(again, n);
    let ids: Vec<u64> = set.current().entries().iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![a, again, b]);
}

#[test]
fn four_kib_pools_round_out_to_sixteen() {
    let (set, log) = set();
    set.register(VA + 0x1000, 0x2000, 7).unwrap();
    assert_eq!(
        events(&log),
        vec![Ev::Pin(host(VA), host(VA) + K16), Ev::Make(host(VA), K16)]
    );
    let snap = set.current();
    let e = &snap.entries()[0];
    // The entry keeps the range the game gave.
    assert_eq!((e.start, e.end), (VA + 0x1000, VA + 0x3000));
    assert_eq!(e.buf.va_start(), VA);
    assert_eq!(e.buf.len(), K16);
    assert_eq!(e.buf.flags(), 7);
    // The offset is from the start of the rounded range.
    assert_eq!(snap.resolve(VA + 0x1000, 1).unwrap().offset, 0x1000);
    // The rounded bytes are not part of the pool.
    assert_eq!(snap.resolve(VA + 0x3000, 1), Err(Miss::NoPool));
    assert_eq!(snap.resolve(VA + 0xfff, 1), Err(Miss::NoPool));
    // An end that is not aligned rounds up.
    set.register(VA + 0x10000, 0x4001, 0).unwrap();
    let ev = events(&log);
    assert_eq!(ev[1], Ev::Make(host(VA + 0x10000), 2 * K16));
}

#[test]
fn pools_that_share_a_rounded_page_keep_their_own_entry() {
    let (set, log) = set();
    set.register(VA, 2 * K4, 0).unwrap();
    set.register(VA + 2 * K4, 2 * K4, 0).unwrap();
    let snap = set.current();
    assert_eq!(snap.entries().len(), 2);
    let (a, b) = (&snap.entries()[0], &snap.entries()[1]);
    assert!(!Arc::ptr_eq(&a.buf, &b.buf));
    // Both buffers cover the same page.
    assert_eq!((a.buf.va_start(), a.buf.len()), (VA, K16));
    assert_eq!((b.buf.va_start(), b.buf.len()), (VA, K16));
    assert_eq!(
        events(&log)
            .iter()
            .filter(|e| matches!(e, Ev::Make(..)))
            .count(),
        2
    );
    // Each resolves to its own entry.
    assert_eq!(snap.resolve(VA + K4, 4).unwrap().pool, 0);
    assert_eq!(snap.resolve(VA + 2 * K4, 4).unwrap().pool, 1);
    assert_eq!(
        snap.resolve(VA + 2 * K4 - 2, 4),
        Err(Miss::CrossesEdge {
            pool_end: VA + 2 * K4
        })
    );
}

#[test]
fn the_generation_counts_every_change() {
    let (set, _) = set();
    assert_eq!(set.current().generation(), 0);
    set.register(VA, K16, 0).unwrap();
    assert_eq!(set.current().generation(), 1);
    // A replacement is one change.
    set.register(VA, K16, 0).unwrap();
    assert_eq!(set.current().generation(), 2);
    set.register(VA + K16, K16, 0).unwrap();
    assert_eq!(set.current().generation(), 3);
    assert!(set.unregister(VA));
    assert_eq!(set.current().generation(), 4);
    // Nothing to remove: no change.
    assert!(!set.unregister(VA));
    assert!(!set.unregister_id(999));
    assert_eq!(set.current().generation(), 4);
    let id = set.current().entries()[0].id;
    assert!(set.unregister_id(id));
    assert_eq!(set.current().generation(), 5);
}

#[test]
fn bad_ranges_are_refused() {
    let (set, log) = set();
    assert!(set.register(VA, 0, 0).is_err());
    assert!(set.register(u64::MAX - 10, 100, 0).is_err());
    assert!(set.register(VA - K16, K16, 0).is_err());
    assert_eq!(set.current().generation(), 0);
    assert!(events(&log).is_empty());
}

#[test]
fn an_old_snapshot_keeps_a_removed_backing_alive() {
    let (set, log) = set();
    set.register(VA, K16, 0).unwrap();
    let old = set.current();
    events(&log);
    assert!(set.unregister(VA));
    assert_eq!(set.current().resolve(VA, 1), Err(Miss::NoPool));
    // The old snapshot still resolves and its backing exists.
    assert!(old.resolve(VA, 1).is_ok());
    assert_eq!(set.drain_graveyard(), Drained::default());
    assert!(events(&log).is_empty());
    let s = set.stats();
    assert_eq!(
        (s.live, s.retired, s.retired_bytes, s.graveyard),
        (0, 1, K16, 0)
    );
    // The last snapshot goes: the backing moves to the graveyard and does not drop.
    drop(old);
    assert!(events(&log).is_empty());
    let s = set.stats();
    assert_eq!((s.retired, s.graveyard, s.graveyard_bytes), (0, 1, K16));
    assert_eq!((s.pins, s.pin_bytes), (1, K16));
}

#[test]
fn the_graveyard_drops_and_unpins_once() {
    let (set, log) = set();
    set.register(VA, K16, 0).unwrap();
    assert!(set.unregister(VA));
    // No snapshot is held, so the backing is in the graveyard now.
    events(&log);
    assert_eq!(
        set.drain_graveyard(),
        Drained {
            count: 1,
            bytes: K16
        }
    );
    // Drop first, then unpin.
    assert_eq!(
        events(&log),
        vec![Ev::Drop(host(VA)), Ev::Unpin(host(VA), host(VA) + K16)]
    );
    assert_eq!(set.drain_graveyard(), Drained::default());
    assert!(events(&log).is_empty());
    assert_eq!(set.stats(), PoolStats::default());
}

#[test]
fn the_order_is_pin_make_drop_unpin() {
    let (set, log) = set();
    set.register(VA, K16, 0).unwrap();
    set.unregister(VA);
    set.drain_graveyard();
    let (s, e) = (host(VA), host(VA) + K16);
    assert_eq!(
        events(&log),
        vec![
            Ev::Pin(s, e),
            Ev::Make(s, K16),
            Ev::Drop(s),
            Ev::Unpin(s, e)
        ]
    );
}

#[test]
fn a_failed_make_registers_the_pool_without_a_backing() {
    let (set, log) = set_with(true);
    let id = set.register(VA, K16, 0).unwrap();
    let (s, e) = (host(VA), host(VA) + K16);
    assert_eq!(events(&log), vec![Ev::Pin(s, e), Ev::Unpin(s, e)]);
    let snap = set.current();
    assert_eq!(snap.entries().len(), 1);
    assert_eq!(snap.entries()[0].id, id);
    assert!(snap.entries()[0].buf.mem().is_none());
    assert_eq!(snap.resolve(VA, 1), Err(Miss::NoBuffer));
    assert_eq!(snap.resolve_start(VA + 8), Err(Miss::NoBuffer));
    // A range outside the pool is still no pool.
    assert_eq!(snap.resolve(VA + K16, 1), Err(Miss::NoPool));
    let s = set.stats();
    assert_eq!((s.live, s.pins), (0, 0));
    // Removing it leaves nothing to drop or unpin.
    assert!(set.unregister(VA));
    assert_eq!(set.drain_graveyard(), Drained::default());
    assert!(events(&log).is_empty());
}

#[test]
fn a_graveyard_that_is_gone_drops_in_place_and_unpins() {
    let (set, log) = set();
    set.register(VA, K16, 0).unwrap();
    let snap = set.current();
    events(&log);
    drop(set);
    assert!(events(&log).is_empty());
    drop(snap);
    let (s, e) = (host(VA), host(VA) + K16);
    assert_eq!(events(&log), vec![Ev::Drop(s), Ev::Unpin(s, e)]);
}

#[test]
fn a_set_that_goes_away_empties_its_graveyard() {
    let (set, log) = set();
    set.register(VA, K16, 0).unwrap();
    set.unregister(VA);
    events(&log);
    drop(set);
    let (s, e) = (host(VA), host(VA) + K16);
    assert_eq!(events(&log), vec![Ev::Drop(s), Ev::Unpin(s, e)]);
}

#[test]
fn whole_mode_resolves_every_address_to_the_one_backing() {
    let log: Log = Arc::default();
    let mem = Fake {
        start: 0x500,
        log: log.clone(),
    };
    let set = PoolSet::whole(VA, 0x10_0000, mem, Arc::new(Pins(log.clone())));
    let snap = set.current();
    assert_eq!(snap.entries().len(), 1);
    for off in [0, 1, 0x4321, 0xf_ffff] {
        let loc = snap.resolve_start(VA + off).unwrap();
        assert_eq!(loc.real, 0x1000_0000 + 0x500 + off);
        assert_eq!(loc.offset, off);
        assert_eq!(loc.room, 0x10_0000 - off);
    }
    assert!(snap.resolve(VA, 0x10_0000).is_ok());
    assert!(matches!(
        snap.resolve(VA, 0x10_0001),
        Err(Miss::CrossesEdge { .. })
    ));
    assert_eq!(snap.resolve(VA - 1, 1), Err(Miss::NoPool));
    assert_eq!(snap.resolve(VA + 0x10_0000, 1), Err(Miss::NoPool));
}

#[test]
fn whole_mode_register_and_unregister_change_nothing() {
    let log: Log = Arc::default();
    let mem = Fake {
        start: 0,
        log: log.clone(),
    };
    let set = PoolSet::whole(VA, 0x10_0000, mem, Arc::new(Pins(log.clone())));
    let before = set.current();
    assert_eq!(set.register(VA + 0x4000, 0x4000, 0), Ok(0));
    assert!(!set.unregister(VA + 0x4000));
    assert!(!set.unregister_id(0));
    assert!(Arc::ptr_eq(&before, &set.current()));
    assert_eq!(set.current().generation(), 0);
    assert_eq!(set.drain_graveyard(), Drained::default());
    // No pin and no make; a bad range is still refused.
    assert!(events(&log).is_empty());
    assert!(set.register(VA, 0, 0).is_err());
    // The backing drops in place with the last snapshot, with no unpin.
    drop(before);
    drop(set);
    assert_eq!(events(&log), vec![Ev::Drop(0)]);
}

#[test]
fn the_counters_follow_the_buffers() {
    let (set, _) = set();
    set.register(VA, K16, 0).unwrap();
    set.register(VA + K16, 2 * K16, 0).unwrap();
    let s = set.stats();
    assert_eq!((s.live, s.live_bytes), (2, 3 * K16));
    assert_eq!((s.pins, s.pin_bytes), (2, 3 * K16));
    assert_eq!((s.retired, s.graveyard), (0, 0));

    let held = set.current();
    set.unregister(VA + K16);
    let s = set.stats();
    assert_eq!((s.live, s.live_bytes), (1, K16));
    assert_eq!((s.retired, s.retired_bytes), (1, 2 * K16));
    assert_eq!((s.pins, s.pin_bytes), (2, 3 * K16));

    drop(held);
    let s = set.stats();
    assert_eq!((s.retired, s.graveyard, s.graveyard_bytes), (0, 1, 2 * K16));
    assert_eq!((s.pins, s.pin_bytes), (2, 3 * K16));

    set.drain_graveyard();
    let s = set.stats();
    assert_eq!((s.graveyard, s.pins, s.pin_bytes), (0, 1, K16));
    assert_eq!((s.live, s.live_bytes), (1, K16));
}

fn overlay_over(snap: &PoolSnapshot<Fake>, members: &[usize], log: &Log) -> PoolSnapshot<Fake> {
    use crate::overlay::{Overlay, Run};
    let mut bits = vec![0u64; 2];
    for &p in members {
        bits[p / 64] |= 1 << (p % 64);
    }
    // Pages 4 to 8 of the guest range have a run; a Fake's real base is 0x1000_0000 plus its start.
    let runs = vec![Run {
        va_start: VA + 4 * K16,
        va_end: VA + 9 * K16,
        mem: Fake {
            start: 0x7_0000,
            log: log.clone(),
        },
    }];
    snap.with_overlay(Arc::new(Overlay::new(VA, Arc::new(bits), runs, None, None, 3)))
}

#[test]
fn a_member_range_resolves_to_the_mirror_run_and_the_rest_to_the_pool() {
    let (set, log) = set();
    set.register(VA, 0x20 * K16, 0).unwrap();
    let base = set.current();
    let snap = overlay_over(&base, &[4, 5, 6], &log);
    assert_eq!(snap.generation(), base.generation());
    // Inside the members: the run's buffer, the offset in it and the room to the end of the run.
    let loc = snap.resolve(VA + 5 * K16 + 0x10, 0x20).unwrap();
    assert_eq!(loc.run, 0);
    assert_eq!(loc.offset, K16 + 0x10);
    assert_eq!(loc.real, 0x1000_0000 + 0x7_0000 + K16 + 0x10);
    assert_eq!(loc.room, 4 * K16 - 0x10);
    // Outside: the pool, as before.
    let out = snap.resolve(VA + 12 * K16, 8).unwrap();
    assert_eq!(out.run, NO_RUN);
    assert_eq!(out, base.resolve(VA + 12 * K16, 8).unwrap());
    // The guest resolve ignores the mirror.
    assert_eq!(snap.resolve_guest(VA + 5 * K16, 8).unwrap().run, NO_RUN);
}

#[test]
fn a_range_from_a_member_into_a_non_member_falls_back_to_guest_and_is_counted() {
    let (set, log) = set();
    set.register(VA, 0x20 * K16, 0).unwrap();
    let base = set.current();
    let snap = overlay_over(&base, &[4, 5], &log);
    let before = crate::overlay::partial();
    let loc = snap.resolve(VA + 5 * K16, 2 * K16).unwrap();
    assert_eq!(loc.run, NO_RUN, "page 6 is no member");
    assert!(crate::overlay::partial() > before);
    // A range that starts outside and reaches into a member is counted too.
    let before = crate::overlay::partial();
    let loc = snap.resolve(VA + 3 * K16, 2 * K16).unwrap();
    assert_eq!(loc.run, NO_RUN);
    assert!(crate::overlay::partial() > before);
    // The start alone has no length to check.
    assert_eq!(snap.resolve_start(VA + 5 * K16).unwrap().run, 0);
}
