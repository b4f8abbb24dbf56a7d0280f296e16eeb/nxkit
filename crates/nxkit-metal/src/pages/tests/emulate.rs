//! Tests of the alias and of the accesses the handler does in place of the thread.

use super::*;

#[test]
fn the_alias_is_the_same_memory_with_its_own_protection() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, _mir, pg) = emulating(4);
    m.store(1, 9, 77);
    let alias = pg.alias_page(1).expect("alias");
    assert_eq!(
        alias[9], 77,
        "a store through the guest mapping shows in the alias"
    );
    // The guest page loses all access; the alias still reads and writes it.
    protect_pages(1, 2, PROT_NONE, false);
    let alias_w = alias.as_ptr().cast_mut();
    // SAFETY: inside the alias page, which is writable.
    unsafe { alias_w.add(10).write_volatile(78) };
    protect_pages(1, 2, PROT_RW, false);
    assert_eq!(
        m.load(1, 10),
        78,
        "a store through the alias shows in guest memory"
    );
    pg.set_mirror(0);
}

#[test]
fn written_ranges_become_blocks_rounded_outward() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, _mir, pg) = emulating(8);
    let ranges = [
        (m.addr(1) + 100, 8),    // block 1
        (m.addr(1) + 127, 2),    // blocks 1 and 2
        (m.addr(2) + 16, 3 * P), // from block 0 of page 2 to block 0 of page 5, partly
        (m.addr(6), 1),          // block 0 of page 6
    ];
    let runs = span_runs(&ranges);
    let masks = range_masks(&runs, &ranges);
    assert_eq!(
        runs,
        vec![(1, 6), (6, 7)]
            .into_iter()
            .fold(Vec::new(), |mut v: Vec<(u32, u32)>, r| {
                match v.last_mut() {
                    Some(l) if l.1 == r.0 => l.1 = r.1,
                    _ => v.push(r),
                }
                v
            })
    );
    let at = |pg: u32| masks[0][(pg - 1) as usize];
    assert_eq!(at(1), [0b110, 0, 0, 0]);
    assert_eq!(at(2), [u64::MAX; 4]);
    assert_eq!(at(3), [u64::MAX; 4]);
    assert_eq!(at(4), [u64::MAX; 4]);
    assert_eq!(at(5), [0b1, 0, 0, 0], "the range ends 16 bytes into page 5");
    assert_eq!(masks[0][5], [0b1, 0, 0, 0]);
    pg.set_mirror(0);
}

/// Loads and stores of several forms at `r` (a region of 256 bytes), from a thread of the test. Returns what the
/// loads got.
///
/// # Safety
/// `r` must be valid for 256 bytes of reads and writes.
#[allow(clippy::type_complexity)]
unsafe fn exercise(r: *mut u8) -> (u64, u64, u64, i64, u64, u64, u64, u64, u64, u64) {
    use std::arch::asm;
    let (x0, x1, x2, sw, b, qlo, qhi, d, pre, post): (
        u64,
        u64,
        u64,
        i64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
    );
    let (mut p1, mut p2) = (r, r);
    // SAFETY: per the caller, the accesses lie inside the region; the registers are declared.
    unsafe {
        asm!(
            "ldr {x0}, [{r}, #0]",
            "ldp {x1}, {x2}, [{r}, #8]",
            "ldrsw {sw}, [{r}, #24]",
            "ldrb {b:w}, [{r}, #28]",
            "ldr q0, [{r}, #32]",
            "fmov {qlo}, d0",
            "mov {qhi}, v0.d[1]",
            "ldr d1, [{r}, #48]",
            "fmov {d}, d1",
            "ldr {pre}, [{p1}, #56]!",
            "ldr {post}, [{p2}], #8",
            r = in(reg) r,
            p1 = inout(reg) p1,
            p2 = inout(reg) p2,
            x0 = out(reg) x0, x1 = out(reg) x1, x2 = out(reg) x2, sw = out(reg) sw, b = out(reg) b,
            qlo = out(reg) qlo, qhi = out(reg) qhi, d = out(reg) d, pre = out(reg) pre, post = out(reg) post,
            out("v0") _, out("v1") _,
            options(nostack),
        );
    }
    assert_eq!(
        p1 as usize,
        r as usize + 56,
        "the pre-index wrote the base back"
    );
    assert_eq!(
        p2 as usize,
        r as usize + 8,
        "the post-index wrote the base back"
    );
    // Stores into the second half of the region.
    let (s0, s1, s2) = (0x1122_3344_5566_7788u64, 0x99aa_bbcc_ddee_ff00u64, 0xa5u64);
    // SAFETY: as above.
    unsafe {
        asm!(
            "str {s0}, [{r}, #128]",
            "stp {s0}, {s1}, [{r}, #136]",
            "fmov d0, {s0}",
            "mov v0.d[1], {s1}",
            "str q0, [{r}, #160]",
            "strh {s2:w}, [{r}, #176]",
            "stur {s0:w}, [{r}, #181]",
            "stp q0, q0, [{r}, #192]",
            "str d0, [{r}, #224]",
            "strb {s2:w}, [{r}, #232]",
            r = in(reg) r,
            s0 = in(reg) s0, s1 = in(reg) s1, s2 = in(reg) s2,
            out("v0") _,
            options(nostack),
        );
    }
    (x0, x1, x2, sw, b, qlo, qhi, d, pre, post)
}

fn le(bytes: impl Fn(u64) -> u8, at: u64, n: u64) -> u64 {
    (0..n).fold(0u64, |a, k| a | u64::from(bytes(at + k)) << (8 * k))
}

#[test]
fn accesses_outside_the_written_bytes_are_done_by_the_handler_on_several_threads_without_a_wait() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = emulating(4);
    for i in 0..P {
        m.store(1, i, pat(i));
    }
    let mut st = Collect::default();
    // The frame may write the first 200 bytes of page 1: blocks 0 to 3.
    let f = pg.flush_mirrored(1, &[], &[(m.addr(1), 200)], None, &mut st);
    assert_eq!((f.new_members, f.written), (1, 1));
    assert_eq!(pg.state_of(m.addr(1)), "gpu(1)");
    let s0 = pg.stats();
    let base = m.addr(1) as usize;
    let handles: Vec<_> = (0..4usize)
        .map(|t| {
            std::thread::spawn(move || {
                let off = 1024 + 1024 * t as u64;
                let mut last = None;
                for _ in 0..50 {
                    // SAFETY: the region lies inside page 1 of the test mapping, which outlives the joined thread.
                    last = Some(unsafe { exercise((base + off as usize) as *mut u8) });
                }
                (off, last)
            })
        })
        .collect();
    for h in handles {
        let (off, last) = h.join().unwrap();
        let (x0, x1, x2, sw, b, qlo, qhi, d, pre, post) = last.unwrap();
        let g = |i: u64| pat(off + i);
        assert_eq!(x0, le(g, 0, 8));
        assert_eq!((x1, x2), (le(g, 8, 8), le(g, 16, 8)));
        assert_eq!(sw, i64::from(le(g, 24, 4) as u32 as i32));
        assert_eq!(b, u64::from(g(28)));
        assert_eq!((qlo, qhi), (le(g, 32, 8), le(g, 40, 8)));
        assert_eq!(d, le(g, 48, 8));
        assert_eq!(pre, le(g, 56, 8));
        assert_eq!(
            post,
            le(g, 0, 8),
            "the post-index load reads before the base moves"
        );
        // The stores landed in guest memory (through the alias).
        let alias = pg.alias_page(1).unwrap();
        let at = |i: u64| alias[(off + i) as usize];
        let word = |i: u64, n: u64| le(at, i, n);
        assert_eq!(word(128, 8), 0x1122_3344_5566_7788);
        assert_eq!(word(136, 8), 0x1122_3344_5566_7788);
        assert_eq!(word(144, 8), 0x99aa_bbcc_ddee_ff00);
        assert_eq!(word(160, 8), 0x1122_3344_5566_7788);
        assert_eq!(word(168, 8), 0x99aa_bbcc_ddee_ff00);
        assert_eq!(word(176, 2), 0x00a5);
        assert_eq!(word(181, 4), 0x5566_7788);
        assert_eq!(word(192, 8), 0x1122_3344_5566_7788);
        assert_eq!(word(216, 8), 0x99aa_bbcc_ddee_ff00);
        assert_eq!(word(224, 8), 0x1122_3344_5566_7788);
        assert_eq!(at(232), 0xa5);
    }
    let s1 = pg.stats();
    let total = |s: &PageStats| s.emulated.iter().sum::<u64>();
    assert_eq!(
        total(&s1) - total(&s0),
        4 * 50 * 16,
        "every access was done by the handler"
    );
    assert_eq!(s1.fallbacks, s0.fallbacks, "none fell back");
    assert_eq!(
        s1.writer_waits, s0.writer_waits,
        "no wait for the writer happened"
    );
    assert_eq!(s1.waits, s0.waits);
    assert_eq!(
        s1.faults, s0.faults,
        "the faults were not served by the ordinary path"
    );
    assert_eq!(pg.state_of(m.addr(1)), "gpu(1)", "the page stayed closed");
    // The GPU wrote the first 200 bytes of the mirror meanwhile.
    for i in 0..200 {
        mir.store(1, i, 0xee);
    }
    // The next flush stages the page with the mask of the stale blocks, and the stores are in the bytes.
    let mut st2 = Collect::default();
    let f2 = pg.flush_mirrored(2, &[], &[], None, &mut st2);
    assert_eq!((f2.staged, f2.emu_staged), (1, 1));
    assert_eq!(st2.skips, vec![(1, [0b1111, 0, 0, 0])]);
    assert_eq!(
        st2.pages[0].1[(1024 + 128) as usize],
        0x88,
        "the staged bytes hold the CPU's stores"
    );
    assert_eq!(pg.stats().emu_staged - s1.emu_staged, 1);
    // Nothing new was stored: the page is not staged again.
    let mut st3 = Collect::default();
    let f3 = pg.flush_mirrored(3, &[], &[], None, &mut st3);
    assert_eq!(f3.staged, 0);
    // The frame completes; a read copies the written blocks only: the stores stay, the GPU's bytes arrive.
    pg.complete(3);
    let sync0 = pg.stats().synced;
    assert_eq!(m.load(1, 5), 0xee, "the GPU's byte is copied back");
    assert_eq!(m.load(1, 199), 0xee);
    assert_eq!(
        m.load(1, 200),
        pat(200),
        "a byte outside the blocks is not touched"
    );
    for t in 0..4u64 {
        let off = 1024 + 1024 * t;
        assert_eq!(
            m.load(1, off + 128),
            0x88,
            "the store survives the copy back"
        );
        assert_eq!(m.load(1, off + 232), 0xa5);
        assert_eq!(
            m.load(1, off + 8),
            pat(off + 8),
            "so do the bytes that were only read"
        );
    }
    assert_eq!(pg.stats().synced - sync0, 1);
    assert_eq!(pg.state_of(m.addr(1)), "clean");
    pg.set_mirror(0);
}
