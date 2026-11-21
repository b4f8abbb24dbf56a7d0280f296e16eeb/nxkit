use super::*;

const BASE: u64 = 0x10_0000_0000;

fn quiet(_: &str) {}

fn heap(size: u64) -> Heap {
    Heap::new(BASE, size, quiet)
}

#[test]
fn small_blocks_come_from_their_class_and_come_back() {
    let h = heap(1 << 24);
    let a = h.alloc(24, 0).unwrap_or(0);
    let b = h.alloc(24, 0).unwrap_or(0);
    assert!(a >= BASE && b >= BASE && a != b);
    assert_eq!(h.size_of(a), Some(32));
    assert_eq!(
        b.abs_diff(a),
        32,
        "two blocks of one class sit side by side"
    );
    assert!(h.free(a));
    assert_eq!(h.alloc(17, 0), Some(a), "a freed block is used again");
    assert_eq!(h.in_use().0, 64);
}

#[test]
fn every_block_keeps_its_alignment() {
    let h = heap(1 << 26);
    for &(size, align) in &[
        (1, 0),
        (40, 32),
        (100, 64),
        (3000, 4096),
        (5000, 4096),
        (20_000, 16),
        (40_000, 0),
        (70_000, 16),
        (200_000, 1 << 20),
    ] {
        let p = h.alloc(size, align).unwrap_or(0);
        assert_ne!(p, 0, "{size} at {align}");
        assert_eq!(p % align.max(16), 0, "{size} at {align}: {p:#x}");
        if size >= SPAN {
            assert_eq!(
                p % SPAN,
                0,
                "{size}: blocks of 64 KiB and more are 64 KiB aligned"
            );
        }
        let got = h.size_of(p).unwrap_or(0);
        assert!(got >= size, "{size}: holds {got}");
        assert_eq!(got, h.quantize(size, align));
    }
}

#[test]
fn large_blocks_coalesce_when_they_come_back() {
    let h = heap(4 * SPAN);
    let a = h.alloc(SPAN, 0).unwrap_or(0);
    let b = h.alloc(SPAN, 0).unwrap_or(0);
    let c = h.alloc(2 * SPAN, 0).unwrap_or(0);
    assert!(a != 0 && b != 0 && c != 0);
    assert_eq!(h.alloc(GRAIN, 0), None, "the range is used up");
    assert!(h.free(a) && h.free(b) && h.free(c));
    assert_eq!(h.in_use().0, 0);
    let all = h.alloc(4 * SPAN, 0);
    assert_eq!(all, Some(BASE), "the three runs are one again");
}

#[test]
fn foreign_pointers_and_interior_pointers_are_refused() {
    let h = heap(1 << 22);
    assert!(!h.free(0x1234));
    assert_eq!(h.size_of(0x1234), None);
    let p = h.alloc(48, 0).unwrap_or(0);
    assert!(!h.free(p + 8), "an interior pointer of a small block");
    let q = h.alloc(SPAN * 2, 0).unwrap_or(0);
    assert!(!h.free(q + GRAIN), "an interior pointer of a large block");
    assert_eq!(h.size_of(q + GRAIN), None);
    assert!(h.free(p) && h.free(q));
}

#[test]
fn the_range_is_cut_to_whole_spans() {
    let h = Heap::new(BASE + 0x100, 3 * SPAN, quiet);
    assert_eq!(h.range(), (BASE + SPAN, BASE + 3 * SPAN));
}

#[test]
fn a_resize_keeps_the_block_only_when_it_fits() {
    assert!(Heap::keeps(BASE, 64, 40, 0));
    assert!(!Heap::keeps(BASE, 64, 80, 0), "too small");
    assert!(!Heap::keeps(BASE + 16, 64, 40, 32), "wrong alignment");
    assert!(Heap::keeps(BASE, 4 * SPAN, 3 * SPAN, 0));
    assert!(
        !Heap::keeps(BASE, 4 * SPAN, SPAN, 0),
        "a large block shrunk to a quarter moves"
    );
}

#[test]
fn threads_share_the_heap() {
    let h = std::sync::Arc::new(heap(1 << 31));
    let workers: Vec<_> = (0..8u64)
        .map(|t| {
            let h = h.clone();
            std::thread::spawn(move || {
                let mut mine = Vec::new();
                for i in 0..2000u64 {
                    let size = 1 + (i * 37 + t * 101) % 50_000;
                    let p = h.alloc(size, 0).unwrap_or(0);
                    assert_ne!(p, 0);
                    mine.push((p, size));
                    if i % 3 == 0 {
                        let (q, _) = mine.swap_remove((i as usize * 7) % mine.len());
                        assert!(h.free(q));
                    }
                }
                mine
            })
        })
        .collect();
    let mut all: Vec<(u64, u64)> = workers
        .into_iter()
        .flat_map(|w| w.join().unwrap_or_default())
        .collect();
    all.sort_unstable();
    for w in all.windows(2) {
        assert!(w[0].0 + w[0].1 <= w[1].0, "blocks overlap: {w:x?}");
    }
    for (p, _) in all {
        assert!(h.free(p));
    }
    assert_eq!(h.in_use().0, 0);
}
