//! The jobs of the pre-pass against pools whose real addresses differ from the guest addresses.

use std::sync::Arc;

use super::*;
use crate::pools::{PagePins, PoolSet};

struct Shifted(u64);

impl Backing for Shifted {
    fn real_base(&self) -> u64 {
        self.0
    }
}

struct NoPins;

impl PagePins for NoPins {
    fn pin(&self, _start: u64, _end: u64) {}
    fn unpin(&self, _start: u64, _end: u64) {}
}

const SHIFT: u64 = 0x1_0000_0000;

/// One pool at `0x4000` of `0x1000` bytes.
fn set() -> PoolSet<Shifted> {
    let set = PoolSet::new(
        0,
        0,
        |h: u64, _l: u64| Ok(Shifted(h + SHIFT)),
        Arc::new(NoPins),
    );
    set.register(0x4000, 0x1000, 0).unwrap();
    set
}

fn multi(indirect: u64, count: u64, max: u32) -> DrawArgs {
    DrawArgs::MultiIndirectCount {
        index: None,
        indirect,
        count,
        max,
        stride: 16,
    }
}

#[test]
fn the_jobs_of_a_multi_draw_hold_real_addresses() {
    let set = set();
    let snap = set.current();
    let mut jobs = Vec::new();
    let r = jobs_of(&*snap, &multi(0x4100, 0x4000, 3), false, &mut jobs);
    assert!(matches!(r, Some(Ok(()))));
    assert_eq!(jobs.len(), 3);
    for (i, j) in jobs.iter().enumerate() {
        assert_eq!(j.rec, 0x4100 + 16 * i as u64 + SHIFT);
        assert_eq!(j.cnt, 0x4000 + SHIFT);
        assert_eq!((j.i, j.words), (i as u32, 4));
    }
}

#[test]
fn a_record_outside_the_pool_skips_the_draw_and_leaves_no_job() {
    let set = set();
    let snap = set.current();
    let mut jobs = Vec::new();
    // record 1 starts at 0x4ff8 and ends past the pool
    let r = jobs_of(&*snap, &multi(0x4fe8, 0x4000, 3), false, &mut jobs);
    assert!(matches!(r, Some(Err(Skip::Buffer))));
    assert!(jobs.is_empty());
    // the count word lies in no pool
    let r = jobs_of(&*snap, &multi(0x4100, 0x9000, 2), false, &mut jobs);
    assert!(matches!(r, Some(Err(Skip::Buffer))));
    assert!(jobs.is_empty());
}

#[test]
fn the_grids_of_an_indirect_mesh_draw_read_the_real_arguments() {
    let set = set();
    let snap = set.current();
    let mut jobs = Vec::new();
    let args = DrawArgs::MeshTasksIndirect { indirect: 0x4200 };
    assert!(matches!(
        jobs_of(&*snap, &args, true, &mut jobs),
        Some(Ok(()))
    ));
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].rec, jobs[0].cnt), (0x4200 + SHIFT, 0x4200 + SHIFT));
    jobs.clear();
    assert!(matches!(
        jobs_of(&*snap, &args, false, &mut jobs),
        Some(Ok(()))
    ));
    assert_eq!(jobs.len(), INDIRECT_SPLITS as usize);
    assert!(jobs.iter().all(|j| j.rec == 0x4200 + SHIFT));
    let far = DrawArgs::MeshTasksIndirect { indirect: 0x9000 };
    assert!(matches!(
        jobs_of(&*snap, &far, false, &mut jobs),
        Some(Err(Skip::Buffer))
    ));
}

#[test]
fn a_draw_with_no_copy_has_no_jobs() {
    let set = set();
    let mut jobs = Vec::new();
    let args = DrawArgs::ArraysIndirect { indirect: 0x4000 };
    assert!(jobs_of(&*set.current(), &args, false, &mut jobs).is_none());
}
