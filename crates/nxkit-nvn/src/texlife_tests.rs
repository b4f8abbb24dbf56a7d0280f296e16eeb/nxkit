//! Tests for the life counters of the texture records.

use crate::backend::PoolInfo;
use crate::front_tests::{HOST, fe, tex, with_pools};
use crate::texlife::Counts;
use crate::tic::TextureView;

#[test]
fn a_re_shared_orphan_stays_alive_when_its_slot_is_reused() {
    let mut f = fe();
    with_pools(&mut f);
    let a = tex(&mut f, 0x10_0000);
    f.register_texture(0, a, TextureView::identity()).unwrap();
    // the game finalizes the texture; the slot keeps the record as an orphan
    assert!(!f.finalize_texture(a).unwrap());
    let desc = f.resources().get(a).unwrap().desc;
    let bytes = f.resources().get(a).unwrap().layout.size;
    assert_eq!(
        f.texlife(),
        Counts {
            inits: 1,
            orphaned: 1,
            orphans: 1,
            orphan_bytes: bytes,
            ..Counts::default()
        }
    );
    // the game initializes the same texture again: the orphan is a live record again
    let (again, shared) = f.create_texture_shared(desc, HOST + 0x10_0000).unwrap();
    assert!(shared && again == a);
    let c = f.texlife();
    assert_eq!((c.reshared, c.orphans, c.orphan_bytes), (1, 0, 0));
    // another texture takes the slot; a texture object names the record, so it must not go
    let b = tex(&mut f, 0x20_0000);
    f.register_texture(0, b, TextureView::identity()).unwrap();
    assert!(f.take_dropped().is_empty());
    assert!(f.resources().get(a).is_ok());
    assert_eq!(f.texlife().released, 0);
    // its last object goes now and no slot holds it
    assert!(f.finalize_texture(a).unwrap());
    assert!(f.resources().get(a).is_err());
    assert_eq!(f.texlife().dropped_now, 1);
}

#[test]
fn the_counters_follow_create_share_finalize_and_release() {
    let mut f = fe();
    with_pools(&mut f);
    let a = tex(&mut f, 0x10_0000);
    assert_eq!(tex(&mut f, 0x10_0000), a);
    f.register_texture(1, a, TextureView::identity()).unwrap();
    // the first finalize only drops one of two texture objects
    assert!(!f.finalize_texture(a).unwrap());
    assert_eq!(f.texlife().orphaned, 0);
    // the last one leaves an orphan because of the slot
    assert!(!f.finalize_texture(a).unwrap());
    let c = f.texlife();
    assert_eq!(
        (c.inits, c.shared, c.reshared, c.orphaned, c.orphans),
        (2, 1, 0, 1, 1)
    );
    // a finalize of a texture without a slot drops it at once
    let b = tex(&mut f, 0x20_0000);
    assert!(f.finalize_texture(b).unwrap());
    assert_eq!(f.texlife().dropped_now, 1);
    // the slot gets another texture: the orphan is released
    let d = tex(&mut f, 0x30_0000);
    f.register_texture(1, d, TextureView::identity()).unwrap();
    assert_eq!(f.take_dropped(), [a]);
    let c = f.texlife();
    assert_eq!((c.released, c.orphans, c.orphan_bytes), (1, 0, 0));
    // three records were made, one is live
    assert_eq!(f.resources().counts(), (3, 1));
}

#[test]
fn the_scan_counts_records_outside_the_live_pools() {
    let mut f = fe();
    with_pools(&mut f);
    let a = tex(&mut f, 0x10_0000);
    tex(&mut f, 0x20_0000);
    let size = f.resources().get(a).unwrap().layout.size;
    f.register_texture(0, a, TextureView::identity()).unwrap();
    f.finalize_texture(a).unwrap();
    // a pool over the first texture only; the pool record is shared by the tests of the crate, so it is removed again
    let pool = HOST + 0x10_0000;
    crate::pools::made(PoolInfo {
        host: pool,
        len: 0x10_0000,
        flags: 0,
    });
    let o = f.scan_outside_pools();
    crate::pools::gone(pool);
    assert_eq!((o.records.count, o.records.bytes), (1, size));
    assert_eq!((o.orphans.count, o.orphans.bytes), (0, 0));
    // without the pool both records are outside, and the orphan too
    let o = f.scan_outside_pools();
    assert_eq!(
        (o.records.count, o.orphans.count, o.orphans.bytes),
        (2, 1, size)
    );
}
