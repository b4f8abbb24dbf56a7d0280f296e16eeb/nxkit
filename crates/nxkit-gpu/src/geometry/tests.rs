//! The address checks of the draws against pools whose real addresses differ from the guest addresses.

use std::sync::Arc;

use super::*;
use crate::pools::{PagePins, PoolSet};

/// A backing whose real address is `SHIFT` above the host address, so a resolve changes the address.
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

/// One pool at `0x4000` of `0x1000` bytes and one at `0x8000` of `0x2000` bytes.
fn set() -> PoolSet<Shifted> {
    let set = PoolSet::new(
        0,
        0,
        |h: u64, _l: u64| Ok(Shifted(h + SHIFT)),
        Arc::new(NoPins),
    );
    set.register(0x4000, 0x1000, 0).unwrap();
    set.register(0x8000, 0x2000, 0).unwrap();
    set
}

fn stream(address: u64, size: u64) -> StreamDetail {
    StreamDetail {
        index: 1,
        address,
        size,
        stride: 16,
        divisor: 0,
    }
}

#[test]
fn the_room_of_an_index_buffer_ends_at_its_pool() {
    let set = set();
    let snap = set.current();
    let (loc, room) = index_room(&*snap, 0x4000).unwrap();
    assert_eq!((loc.real, room), (0x4000 + SHIFT, 0x1000));
    let (loc, room) = index_room(&*snap, 0x4c00).unwrap();
    assert_eq!((loc.real, room), (0x4c00 + SHIFT, 0x400));
    // the last byte of a pool is in it, its end and the gap after it are not
    assert_eq!(index_room(&*snap, 0x4fff).unwrap().1, 1);
    assert_eq!(index_room(&*snap, 0x5000).err(), Some(Skip::Buffer));
    assert_eq!(index_room(&*snap, 0x3fff).err(), Some(Skip::Buffer));
}

#[test]
fn the_room_is_cut_to_what_a_32_bit_size_holds() {
    let set = PoolSet::new(
        0,
        0,
        |h: u64, _l: u64| Ok(Shifted(h + SHIFT)),
        Arc::new(NoPins),
    );
    set.register(0x1_0000_0000, 0x2_0000_0000, 0).unwrap();
    let (_, room) = index_room(&*set.current(), 0x1_0000_0000).unwrap();
    assert_eq!(room, u64::from(u32::MAX));
}

#[test]
fn a_stream_longer_than_its_pool_is_clamped_and_one_outside_skips() {
    let set = set();
    let snap = set.current();
    let loc = stream_loc(&*snap, &stream(0x4800, 0x10_0000)).unwrap();
    assert_eq!((loc.real, loc.room), (0x4800 + SHIFT, 0x800));
    // a size of 0 still needs its first byte in a pool
    assert!(stream_loc(&*snap, &stream(0x8000, 0)).is_ok());
    assert_eq!(
        stream_loc(&*snap, &stream(0x6000, 16)).err(),
        Some(Skip::Buffer)
    );
    assert_eq!(
        stream_loc(&*snap, &stream(0x5000, 16)).err(),
        Some(Skip::Buffer)
    );
}

#[test]
fn indices_resolve_exactly_and_must_stay_in_one_pool() {
    let set = set();
    let snap = set.current();
    let loc = index_loc(&*snap, 0x4000, 0x400, 4).unwrap();
    assert_eq!(loc.real, 0x4000 + SHIFT);
    assert_eq!(
        index_loc(&*snap, 0x4000, 0x401, 4).err(),
        Some(Skip::Buffer)
    );
    // a range that crosses the end of its pool
    assert!(index_loc(&*snap, 0x4ff0, 16, 2).is_err());
}

#[test]
fn indirect_arguments_need_a_pool_and_four_byte_alignment() {
    let set = set();
    let snap = set.current();
    let loc = indirect_args(&*snap, 0x4010, 20).unwrap();
    assert_eq!(loc.real, 0x4010 + SHIFT);
    assert_eq!(indirect_args(&*snap, 0x4012, 16).err(), Some(Skip::Buffer));
    assert_eq!(indirect_args(&*snap, 0x4ff8, 20).err(), Some(Skip::Buffer));
    assert_eq!(indirect_args(&*snap, 0x7000, 16).err(), Some(Skip::Buffer));
    // the words must be aligned where the GPU reads them, too
    let odd = PoolSet::new(0, 0, |h: u64, _l: u64| Ok(Shifted(h + 2)), Arc::new(NoPins));
    odd.register(0x4000, 0x1000, 0).unwrap();
    assert_eq!(
        indirect_args(&*odd.current(), 0x4000, 16).err(),
        Some(Skip::Buffer)
    );
}
