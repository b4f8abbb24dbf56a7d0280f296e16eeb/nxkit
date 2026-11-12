//! Tests of the pool table layout, the hints, the write log and the learned ranges.

use super::*;

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[test]
fn the_table_has_the_layout_of_the_abi() {
    let mib = 1u64 << 20;
    let t = build_table([
        (0x10 * mib, 0x14 * mib, 0),
        (0x40 * mib, 0x41 * mib, 0x1000),
    ]);
    assert_eq!(t.bytes.len(), 8192 * 32);
    // entry 0 is empty, unused entries start at !0
    assert_eq!(&t.bytes[..32], &[0u8; 32]);
    assert_eq!(u64_at(&t.bytes, 3 * 32), u64::MAX);
    assert_eq!(u64_at(&t.bytes, 3 * 32 + 8), 0);
    // entry 1 and 2: start, end, delta
    assert_eq!(
        (
            u64_at(&t.bytes, 32),
            u64_at(&t.bytes, 40),
            u64_at(&t.bytes, 48)
        ),
        (0x10 * mib, 0x14 * mib, 0)
    );
    assert_eq!(
        (
            u64_at(&t.bytes, 64),
            u64_at(&t.bytes, 72),
            u64_at(&t.bytes, 80)
        ),
        (0x40 * mib, 0x41 * mib, 0x1000)
    );
    assert_eq!(
        t.ranges,
        vec![(0, 0), (0x10 * mib, 0x14 * mib), (0x40 * mib, 0x41 * mib)]
    );
}

#[test]
fn an_overlapping_pool_keeps_the_part_past_the_earlier_one() {
    let t = build_table([(100, 200, 1), (150, 300, 2), (120, 180, 3), (400, 400, 4)]);
    // the third pool is wholly inside the second range, the fourth is empty
    assert_eq!(t.ranges, vec![(0, 0), (100, 200), (200, 300)]);
    assert_eq!(u64_at(&t.bytes, 2 * 32 + 16), 2);
}

#[test]
fn hints_name_the_entry_that_holds_a_whole_chunk() {
    let size = 1u64 << HINT_SHIFT;
    // chunks 3 and 4 lie inside [2.5 chunks, 5 chunks); chunk 2 is cut by the start
    let t = build_table([(size * 5 / 2, size * 5, 0)]);
    let hint_of = |c: u64| u32_at(&t.bytes, (c as usize % ENTRIES) * ENTRY_BYTES + 24);
    assert_eq!(hint_of(2), 0);
    assert_eq!(hint_of(3), 3 << 13 | 1);
    assert_eq!(hint_of(4), 4 << 13 | 1);
    assert_eq!(hint_of(5), 0);
}

#[test]
fn a_full_table_counts_the_pools_it_leaves_out() {
    let t = build_table((0..ENTRIES as u64 + 5).map(|i| (i * 0x1000, i * 0x1000 + 0x800, 0)));
    assert_eq!(t.ranges.len(), ENTRIES);
    assert_eq!(t.over, 6);
}

#[test]
fn the_log_decodes_to_the_ranges_of_the_set_bits() {
    let ranges = vec![(0, 0), (10, 20), (30, 40), (50, 60)];
    let mut words = vec![0u32; ENTRIES / 32];
    words[0] = 0b1111;
    // a bit past the table is ignored
    words[3] = 1 << 7;
    assert_eq!(decode(&words, &ranges), vec![(10, 20), (30, 40), (50, 60)]);
    assert_eq!(LOG_BYTES, 1024);
}

#[test]
fn the_pools_the_snapshot_does_not_resolve_are_counted() {
    let set = shifted_set();
    set.register(0x4000, 0x4000, 0).unwrap();
    let sorted = [
        (0x4000, 0x8000),
        (0x10_0000, 0x10_1000),
        (0x20_0000, 0x20_1000),
    ];
    let (resolved, unresolved) = resolve_all(&sorted, &*set.current());
    assert_eq!(resolved, vec![(0x4000, 0x8000, SHIFT)]);
    assert_eq!(unresolved, 2);
}

/// A backing whose real address is a fixed distance from the host address, so a table delta is visible.
struct Shifted(u64);

impl Backing for Shifted {
    fn real_base(&self) -> u64 {
        self.0
    }
}

struct NoPins;

impl crate::pools::PagePins for NoPins {
    fn pin(&self, _start: u64, _end: u64) {}
    fn unpin(&self, _start: u64, _end: u64) {}
}

const SHIFT: u64 = 0x1_0000_0000;

fn shifted_set() -> crate::pools::PoolSet<Shifted> {
    crate::pools::PoolSet::new(
        0,
        0,
        |host: u64, _len: u64| Ok(Shifted(host + SHIFT)),
        Arc::new(NoPins),
    )
}

fn delta_of_entry(buf: &DataBuffer, entry: usize) -> u64 {
    let mut b = [0u8; 8];
    buf.read((entry * ENTRY_BYTES + 16) as u64, &mut b).unwrap();
    u64::from_le_bytes(b)
}

#[test]
fn the_table_takes_the_delta_of_a_pool_from_the_snapshot() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let set = shifted_set();
    set.register(0x4000, 0x4000, 0).unwrap();
    let mut g = Gmem::new(&ctx).unwrap();
    g.add_pool(0x4000, 0x4000);
    // a pool that the snapshot does not hold is left out
    g.add_pool(0x10_0000, 0x1000);
    let buf = g.refresh(&ctx, &*set.current()).unwrap();
    assert_eq!(g.ranges(), Arc::new(vec![(0, 0), (0x4000, 0x8000)]));
    assert_eq!(delta_of_entry(&buf, 1), SHIFT);
}

#[test]
fn the_delta_of_the_whole_mode_is_the_real_address_minus_the_virtual_one() {
    use nxkit_metal::GUEST_VA_BASE;
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    // The real address of the buffer lies below the window, so the delta is negative and wraps.
    let real = 0x15_0000_0000u64;
    let snap = PoolSnapshot::whole(GUEST_VA_BASE, 0x10_0000, Shifted(real));
    let mut g = Gmem::new(&ctx).unwrap();
    let start = GUEST_VA_BASE + 0x4000;
    g.add_pool(start, 0x4000);
    let buf = g.refresh(&ctx, &snap).unwrap();
    assert_eq!(g.ranges(), Arc::new(vec![(0, 0), (start, start + 0x4000)]));
    let delta = delta_of_entry(&buf, 1);
    assert_eq!(delta, real.wrapping_sub(GUEST_VA_BASE));
    assert_ne!(delta, 0);
    // The shader adds the delta to the guest address, modulo 2^64.
    assert_eq!((start + 0x10).wrapping_add(delta), real + 0x4010);
}

#[test]
fn the_whole_window_fits_the_chunk_numbers_of_the_hints() {
    use nxkit_metal::{GUEST_VA_BASE, GUEST_VA_MAX};
    // A hint holds 19 bits of the chunk number, so the last chunk of the window must fit.
    const { assert!((GUEST_VA_MAX - 1) >> HINT_SHIFT < 1 << (32 - HINT_INDEX_BITS)) };
    let size = 1u64 << HINT_SHIFT;
    let t = build_table([(GUEST_VA_BASE, GUEST_VA_BASE + 4 * size, 7)]);
    let c = GUEST_VA_BASE >> HINT_SHIFT;
    let hint = u32_at(&t.bytes, (c as usize % ENTRIES) * ENTRY_BYTES + 24);
    assert_eq!(u64::from(hint), c << 13 | 1);
}

#[test]
fn the_table_is_built_again_only_when_a_generation_changed() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let set = shifted_set();
    set.register(0x4000, 0x4000, 0).unwrap();
    let mut g = Gmem::new(&ctx).unwrap();
    g.add_pool(0x4000, 0x4000);
    let snap = set.current();
    let first = g.refresh(&ctx, &*snap).unwrap();
    // the same pools and the same generation: the same buffer
    let again = g.refresh(&ctx, &*snap).unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    let same = g.refresh(&ctx, &*set.current()).unwrap();
    assert!(Arc::ptr_eq(&first, &same));
    // a new snapshot generation builds it again
    set.register(0x10_0000, 0x4000, 0).unwrap();
    let newer = g.refresh(&ctx, &*set.current()).unwrap();
    assert!(!Arc::ptr_eq(&first, &newer));
    let held = g.refresh(&ctx, &*set.current()).unwrap();
    assert!(Arc::ptr_eq(&newer, &held));
    // so does a change of the pools of the table
    g.add_pool(0x10_0000, 0x4000);
    let last = g.refresh(&ctx, &*set.current()).unwrap();
    assert!(!Arc::ptr_eq(&newer, &last));
    assert_eq!(g.ranges().len(), 3);
}

#[test]
fn logs_come_back_through_the_table_of_their_batch() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let g = Gmem::new(&ctx).unwrap();
    let logs = g.logs();
    logs.begin(1, Arc::new(vec![(0, 0), (0x100, 0x200), (0x300, 0x400)]));
    let a = logs.take(1);
    let b = logs.take(1);
    assert_eq!(b - a, LOG_BYTES);
    // the shader of the second draw stored through entry 2
    logs.buffer
        .write(b - logs.buffer.gpu_address(), &4u32.to_le_bytes())
        .unwrap();
    assert_eq!(logs.drain(1), vec![(0x300, 0x400)]);
    // drained and cleared
    assert!(logs.drain(1).is_empty());
    assert_eq!(logs.take(1), a);
}

#[derive(Clone)]
struct Fake(u64);

impl Backing for Fake {
    fn real_base(&self) -> u64 {
        self.0
    }
}

#[test]
fn a_pool_is_split_at_its_member_pages_with_the_delta_of_the_mirror_run() {
    use crate::overlay::{Overlay, PAGE, Run};
    const VA: u64 = 0xc0_0000_0000;
    let snap = PoolSnapshot::whole(VA, 64 * PAGE, Fake(0x9000_0000_0000));
    // Pages 10, 11 and 14 are members; the run for them starts at page 8.
    let mut bits = vec![0u64; 1];
    for p in [10, 11, 14] {
        bits[0] |= 1 << p;
    }
    let runs = vec![Run {
        va_start: VA + 8 * PAGE,
        va_end: VA + 16 * PAGE,
        mem: Fake(0x7000_0000_0000),
    }];
    let snap = snap.with_overlay(std::sync::Arc::new(Overlay::new(
        VA,
        std::sync::Arc::new(bits),
        runs,
        None,
        None,
        1,
    )));
    // One registered pool, from page 9 to page 20.
    let (pieces, unresolved) = resolve_all(&[(VA + 9 * PAGE, VA + 20 * PAGE)], &snap);
    assert_eq!(unresolved, 0);
    let guest = 0x9000_0000_0000u64.wrapping_sub(VA);
    // The mirror address of a byte is the run base plus its offset in the run: delta = run_real - run_va.
    let mirror_delta = 0x7000_0000_0000u64.wrapping_sub(VA + 8 * PAGE);
    assert_eq!(
        pieces,
        vec![
            (VA + 9 * PAGE, VA + 10 * PAGE, guest),
            (VA + 10 * PAGE, VA + 12 * PAGE, mirror_delta),
            (VA + 12 * PAGE, VA + 14 * PAGE, guest),
            (VA + 14 * PAGE, VA + 15 * PAGE, mirror_delta),
            (VA + 15 * PAGE, VA + 20 * PAGE, guest),
        ]
    );
    // The table built from the pieces holds them in order.
    let t = build_table(pieces);
    assert_eq!(t.ranges.len(), 6);
}
