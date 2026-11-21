use super::*;

const WIN: u64 = 0xE0_0000_0000;
const PHYS: u64 = 0xC0_1000_0000;

fn space() -> VirtualSpace {
    VirtualSpace::new(WIN, 0x100_0000).unwrap()
}

fn ga(a: u64) -> GpuAddr {
    GpuAddr::new(a).unwrap()
}

/// Physical memory for the gathers: byte `PHYS + i` holds `i as u8 ^ (i >> 12) as u8`.
fn phys_byte(addr: u64) -> u8 {
    let i = addr - PHYS;
    (i as u8) ^ ((i >> 12) as u8)
}

fn reader(calls: &mut Vec<(u64, usize)>) -> impl FnMut(u64, &mut [u8]) -> bool + '_ {
    move |at, out| {
        calls.push((at, out.len()));
        for (k, b) in out.iter_mut().enumerate() {
            *b = phys_byte(at + k as u64);
        }
        true
    }
}

#[test]
fn a_window_must_be_aligned_and_fit_in_40_bits() {
    assert!(VirtualSpace::new(WIN + 0x1000, 0x10_0000).is_err());
    assert!(VirtualSpace::new(WIN, 0).is_err());
    assert!(VirtualSpace::new(0xFF_FFFF_0000, 0x2_0000).is_err());
    assert!(VirtualSpace::new(0xFF_FFFF_0000, 0x1_0000).is_ok());
    assert_eq!(space().window(), (WIN, 0x100_0000));
}

#[test]
fn pools_get_aligned_disjoint_ranges_and_a_released_range_comes_back_last() {
    let mut s = space();
    let a = s.reserve(0x2_1000).unwrap();
    let b = s.reserve(0x1000).unwrap();
    assert_eq!(a.get(), WIN);
    assert_eq!(b.get(), WIN + 0x3_0000);
    s.release(a);
    let c = s.reserve(0x1000).unwrap();
    // the ring goes on past b before it reuses a's range
    assert_eq!(c.get(), WIN + 0x4_0000);
    assert!(s.tables().contains(b.get()));
    assert!(!s.tables().contains(a.get()));
    assert_eq!(s.tables().find(b.get() + 0xFFF).unwrap().base(), b);
    // past the size of b (one page) is no pool, even inside its aligned range
    assert!(!s.tables().contains(b.get() + 0x1000));
}

#[test]
fn the_ring_wraps_around_and_a_full_window_refuses() {
    let mut s = VirtualSpace::new(WIN, 0x4_0000).unwrap();
    let a = s.reserve(0x1_0000).unwrap();
    let _b = s.reserve(0x2_0000).unwrap();
    let _c = s.reserve(0x1_0000).unwrap();
    assert!(s.reserve(1).is_err());
    s.release(a);
    assert_eq!(s.reserve(0x1_0000).unwrap(), a);
}

#[test]
fn a_mapping_fills_the_table_and_counts_generations() {
    let mut s = space();
    let base = s.reserve(0x10_0000).unwrap();
    let (at, len) = s
        .map(base, 0x2000, 0x3000, Some(ga(PHYS + 0x1_0000)))
        .unwrap();
    assert_eq!((at.get(), len), (base.get() + 0x2000, 0x3000));
    assert_eq!(s.generation(), 1);
    let t = s.tables().find(base.get()).unwrap();
    assert_eq!(t.page(0x1000), Pte::default());
    assert_eq!(
        t.page(0x2000),
        Pte {
            phys: PHYS + 0x1_0000,
            generation: 1
        }
    );
    assert_eq!(t.page(0x4000).phys, PHYS + 0x1_2000);
    assert_eq!(t.page(0x5000).phys, 0);
    assert_eq!(t.generation(0, 0x2000), 0);
    assert_eq!(t.generation(0x1FFF, 2), 1);
    assert_eq!(
        s.tables().resolve(base.get() + 0x3123),
        Some(PHYS + 0x1_1123)
    );
    assert_eq!(s.tables().resolve(base.get() + 0x5000), None);
}

#[test]
fn a_remap_replaces_the_pages_and_an_unmap_clears_them() {
    let mut s = space();
    let base = s.reserve(0x10_0000).unwrap();
    s.map(base, 0, 0x4000, Some(ga(PHYS))).unwrap();
    s.map(base, 0x1000, 0x1000, Some(ga(PHYS + 0x8_0000)))
        .unwrap();
    let t = s.tables();
    assert_eq!(t.resolve(base.get()), Some(PHYS));
    assert_eq!(t.resolve(base.get() + 0x1000), Some(PHYS + 0x8_0000));
    assert_eq!(t.find(base.get()).unwrap().page(0x1000).generation, 2);
    assert_eq!(t.find(base.get()).unwrap().page(0x2000).generation, 1);
    s.map(base, 0x2000, 0x2000, None).unwrap();
    let t = s.tables().find(base.get()).unwrap();
    assert_eq!(
        t.page(0x2000),
        Pte {
            phys: 0,
            generation: 3
        }
    );
    assert_eq!(t.page(0x3000).phys, 0);
    assert_eq!(t.page(0).phys, PHYS);
    // the remapped and unmapped physical pages are no longer aliases
    let mut out = Vec::new();
    s.aliases(PHYS + 0x1000, 0x3000, &mut out);
    assert!(out.is_empty(), "{out:x?}");
    s.aliases(PHYS + 0x8_0010, 0x10, &mut out);
    assert_eq!(out, vec![(ga(base.get() + 0x1010), 0x10)]);
}

#[test]
fn a_request_off_the_4k_grid_or_past_the_pool_is_refused_and_changes_nothing() {
    let mut s = space();
    let base = s.reserve(0x4000).unwrap();
    assert!(s.map(base, 0x800, 0x1000, Some(ga(PHYS))).is_err());
    assert!(s.map(base, 0, 0x1800, Some(ga(PHYS))).is_err());
    assert!(s.map(base, 0, 0x1000, Some(ga(PHYS + 0x10))).is_err());
    assert!(s.map(base, 0x3000, 0x2000, Some(ga(PHYS))).is_err());
    assert!(
        s.map(ga(WIN + 0x10_0000), 0, 0x1000, Some(ga(PHYS)))
            .is_err()
    );
    assert_eq!(s.generation(), 0);
    assert_eq!(s.tables().resolve(base.get()), None);
}

#[test]
fn a_snapshot_keeps_the_table_of_its_moment() {
    let mut s = space();
    let base = s.reserve(0x4000).unwrap();
    s.map(base, 0, 0x1000, Some(ga(PHYS))).unwrap();
    let snap = s.snapshot();
    s.map(base, 0, 0x1000, None).unwrap();
    let b = s.reserve(0x1000).unwrap();
    assert_eq!(snap.resolve(base.get()), Some(PHYS));
    assert!(!snap.contains(b.get()));
    assert_eq!(s.tables().resolve(base.get()), None);
}

#[test]
fn a_gather_reads_runs_of_contiguous_pages_and_zero_for_holes() {
    let mut s = space();
    let base = s.reserve(0x10_0000).unwrap();
    // pages 0 and 1 contiguous, page 2 a hole, page 3 elsewhere, page 4 right behind page 3
    s.map(base, 0, 0x2000, Some(ga(PHYS + 0x4000))).unwrap();
    s.map(base, 0x3000, 0x2000, Some(ga(PHYS + 0x1_0000)))
        .unwrap();
    let mut out = vec![0xAAu8; 0x5000];
    let mut calls = Vec::new();
    let g = s
        .tables()
        .gather(base, &mut out, reader(&mut calls))
        .unwrap();
    assert_eq!(
        calls,
        vec![(PHYS + 0x4000, 0x2000), (PHYS + 0x1_0000, 0x2000)]
    );
    assert_eq!(
        g,
        Gathered {
            mapped: 0x4000,
            unmapped: 0x1000,
            missing: 0,
            reads: 2
        }
    );
    assert_eq!(out[0x10], phys_byte(PHYS + 0x4010));
    assert_eq!(out[0x1FFF], phys_byte(PHYS + 0x5FFF));
    assert!(out[0x2000..0x3000].iter().all(|&b| b == 0));
    assert_eq!(out[0x3001], phys_byte(PHYS + 0x1_0001));
    assert_eq!(out[0x4FFF], phys_byte(PHYS + 0x1_1FFF));
}

#[test]
fn a_gather_of_partial_pages_reads_only_the_bytes_inside_the_range() {
    let mut s = space();
    let base = s.reserve(0x10_0000).unwrap();
    s.map(base, 0, 0x2000, Some(ga(PHYS))).unwrap();
    s.map(base, 0x2000, 0x1000, Some(ga(PHYS + 0x9000)))
        .unwrap();
    let mut out = vec![0u8; 0x1800];
    let mut calls = Vec::new();
    let start = base.offset(0xC00).unwrap();
    s.tables()
        .gather(start, &mut out, reader(&mut calls))
        .unwrap();
    // 0xC00..0x2000 is one physical run, 0x2000..0x2400 the next one
    assert_eq!(calls, vec![(PHYS + 0xC00, 0x1400), (PHYS + 0x9000, 0x400)]);
    assert_eq!(out[0], phys_byte(PHYS + 0xC00));
    assert_eq!(out[0x13FF], phys_byte(PHYS + 0x1FFF));
    assert_eq!(out[0x1400], phys_byte(PHYS + 0x9000));
    // inside one page
    let mut small = [0u8; 0x10];
    calls.clear();
    s.tables()
        .gather(base.offset(0x2100).unwrap(), &mut small, reader(&mut calls))
        .unwrap();
    assert_eq!(calls, vec![(PHYS + 0x9100, 0x10)]);
}

#[test]
fn a_gather_zeroes_what_the_reader_cannot_supply_and_refuses_a_range_past_the_pool() {
    let mut s = space();
    let base = s.reserve(0x2000).unwrap();
    s.map(base, 0, 0x2000, Some(ga(PHYS))).unwrap();
    let mut out = vec![0xFFu8; 0x2000];
    let g = s.tables().gather(base, &mut out, |_, _| false).unwrap();
    assert_eq!(g.missing, 0x2000);
    assert!(out.iter().all(|&b| b == 0));
    let mut big = vec![0u8; 0x2001];
    assert!(s.tables().gather(base, &mut big, |_, _| true).is_err());
    assert!(s.tables().gather(ga(PHYS), &mut out, |_, _| true).is_err());
}

#[test]
fn aliases_name_every_virtual_page_behind_a_physical_write() {
    let mut s = space();
    let a = s.reserve(0x4000).unwrap();
    let b = s.reserve(0x4000).unwrap();
    s.map(a, 0, 0x2000, Some(ga(PHYS))).unwrap();
    // the same physical page also in b, once
    s.map(b, 0x1000, 0x1000, Some(ga(PHYS + 0x1000))).unwrap();
    let mut out = Vec::new();
    s.aliases(PHYS + 0x800, 0x1000, &mut out);
    out.sort();
    assert_eq!(
        out,
        vec![(ga(a.get() + 0x800), 0x1000), (ga(b.get() + 0x1000), 0x800)]
    );
    out.clear();
    s.aliases(PHYS + 0x2000, 0x1000, &mut out);
    assert!(out.is_empty());
    s.release(a);
    s.aliases(PHYS, 0x2000, &mut out);
    assert_eq!(out, vec![(ga(b.get() + 0x1000), 0x1000)]);
}
