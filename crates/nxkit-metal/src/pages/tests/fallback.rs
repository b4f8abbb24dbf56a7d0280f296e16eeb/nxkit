//! Tests of the accesses that wait, of the forms across a page border and of the emulated atomics.

use super::*;

#[test]
fn an_access_that_touches_the_written_bytes_waits_and_is_counted() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = emulating(4);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(7, &[], &[(m.addr(1) + 64, 64)], None, &mut st);
    mir.store(1, 70, 1);
    let s0 = pg.stats();
    let base = m.addr(1) as usize;
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d2 = done.clone();
    // An 8-byte load whose last byte is in the written block (bytes 56 to 63 are block 0, 64 to 127 block 1).
    let reader = std::thread::spawn(move || {
        let v: u64;
        // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
        unsafe {
            std::arch::asm!("ldr {v}, [{p}, #60]", p = in(reg) base, v = out(reg) v, options(nostack));
        }
        d2.store(true, Ordering::Release);
        v
    });
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(
        !done.load(Ordering::Acquire),
        "an access that touches written bytes waits for the frame"
    );
    mir.store(1, 66, 0x42);
    pg.complete(7);
    let v = reader.join().unwrap();
    assert_eq!((v >> 48) & 0xff, 0x42, "the load saw the GPU's byte");
    let s1 = pg.stats();
    assert_eq!(
        s1.fallbacks[Reason::Overlap as usize] - s0.fallbacks[Reason::Overlap as usize],
        1
    );
    assert_eq!(s1.writer_waits - s0.writer_waits, 1);
    assert_eq!(s1.emulated, s0.emulated);
    pg.set_mirror(0);
}

#[test]
fn what_cannot_be_emulated_waits_and_is_counted_by_class() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = emulating(4);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(9, &[], &[(m.addr(1), 64)], None, &mut st);
    mir.store(1, 0, 1);
    let s0 = pg.stats();
    let base = m.addr(1) as usize + 1024;
    let reader = std::thread::spawn(move || {
        // SAFETY: inside page 1 of the test mapping, which outlives the joined thread; LD2 is not emulated.
        unsafe {
            std::arch::asm!("ld2 {{v0.16b, v1.16b}}, [{p}]", p = in(reg) base, out("v0") _, out("v1") _, options(nostack));
        }
        0u64
    });
    std::thread::sleep(std::time::Duration::from_millis(40));
    pg.complete(9);
    let _ = reader.join().unwrap();
    let s1 = pg.stats();
    assert_eq!(
        s1.fallbacks[Reason::Structure as usize] - s0.fallbacks[Reason::Structure as usize],
        1
    );
    assert_eq!(
        s1.writer_waits - s0.writer_waits,
        1,
        "it waited for the writer"
    );
    assert_eq!(s1.emulated, s0.emulated);
    pg.set_mirror(0);
}

#[test]
fn with_the_emulation_off_every_access_waits() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, _mir, pg) = emulating(4);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(11, &[], &[(m.addr(1), 64)], None, &mut st);
    pg.set_emulation(false);
    assert!(!pg.emulating());
    let base = m.addr(1) as usize + 1024;
    let s0 = pg.stats();
    let reader = std::thread::spawn(move || {
        // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
        unsafe { (base as *const u8).read_volatile() }
    });
    std::thread::sleep(std::time::Duration::from_millis(40));
    pg.complete(11);
    reader.join().unwrap();
    let s1 = pg.stats();
    assert_eq!(s1.writer_waits - s0.writer_waits, 1);
    assert_eq!(s1.emulated, s0.emulated);
    pg.set_emulation(true);
    pg.set_mirror(0);
}

#[test]
fn a_store_across_the_page_border_and_a_lane_store_are_done_by_the_handler_without_a_wait() {
    use std::arch::asm;
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, _mir, pg) = emulating(4);
    let mut st = Collect::default();
    // Page 1 is in flight (bytes 0 to 255 are the frame's), page 2 is clean and tracked.
    let _ = pg.flush_mirrored(1, &[(m.addr(2), P)], &[(m.addr(1), 256)], None, &mut st);
    let s0 = pg.stats();
    let border = (m.addr(1) + P) as usize;
    // SAFETY: the pair lies on the border of pages 1 and 2 of the test mapping; the handler does the store.
    unsafe {
        asm!("stp {a}, {b}, [{p}]", p = in(reg) border - 8, a = in(reg) 0x1122_3344_5566_7788u64,
            b = in(reg) 0x99aa_bbcc_ddee_ff00u64, options(nostack));
        // `st1 {v0.s}[1], [x]` in the page in flight, outside the frame's bytes.
        asm!("movi v0.4s, #7", "st1 {{v0.s}}[1], [{p}]", p = in(reg) m.addr(1) as usize + 1024, out("v0") _,
            options(nostack));
    }
    let s1 = pg.stats();
    assert_eq!(s1.writer_waits, s0.writer_waits, "no wait");
    assert_eq!(
        s1.fallbacks[Reason::Mismatch as usize],
        s0.fallbacks[Reason::Mismatch as usize]
    );
    assert_eq!(
        s1.emulated[Form::St1 as usize] - s0.emulated[Form::St1 as usize],
        1
    );
    let a1 = pg.alias_page(1).unwrap();
    let a2 = pg.alias_page(2).unwrap();
    assert_eq!(
        &a1[(P - 8) as usize..],
        &0x1122_3344_5566_7788u64.to_le_bytes()
    );
    assert_eq!(&a2[..8], &0x99aa_bbcc_ddee_ff00u64.to_le_bytes());
    assert_eq!(&a1[1024..1028], &7u32.to_le_bytes());
    // The flush stages the clean page that the handler stored to.
    let mut st2 = Collect::default();
    let f = pg.flush_mirrored(2, &[], &[], None, &mut st2);
    assert!(f.emu_staged >= 2, "pages 1 and 2 were stored to: {f:?}");
    pg.complete(1);
    pg.complete(2);
}

#[test]
fn atomics_exclusives_and_ld1_outside_the_written_bytes_are_done_by_the_handler_and_sum_correctly()
{
    use std::arch::asm;
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, _mir, pg) = emulating(4);
    let mut st = Collect::default();
    // The frame may write bytes 0 to 255; everything below is outside.
    let _ = pg.flush_mirrored(1, &[], &[(m.addr(1), 256)], None, &mut st);
    let s0 = pg.stats();
    let base = m.addr(1) as usize;
    const N: u64 = 300;
    // The sources of the LD1 copies, written through the alias (the page has no access).
    let a0 = pg.alias_page(1).unwrap().as_ptr().cast_mut();
    for k in 0..4 * 64 {
        // SAFETY: inside the alias page, which is writable.
        unsafe { a0.add(1024 + 128 + k).write(k as u8 ^ 0x3c) };
    }
    let handles: Vec<_> = (0..4u64)
        .map(|t| {
            std::thread::spawn(move || {
                let p = base + 1024;
                for i in 0..N {
                    // SAFETY: the counters lie inside page 1 of the test mapping, which outlives the joined threads;
                    // the offsets are aligned for their accesses.
                    unsafe {
                        // LDADD (add one), through the acquire-release form.
                        asm!(".arch_extension lse", "ldaddal {one}, {old}, [{p}]",
                            p = in(reg) p, one = in(reg) 1u64, old = out(reg) _, options(nostack));
                        // STADD of a word (no result).
                        asm!(".arch_extension lse", "stadd {one:w}, [{p}]",
                            p = in(reg) p + 8, one = in(reg) 1u32, options(nostack));
                        // LDXR/STXR increment loop.
                        asm!("2:", "ldaxr {t}, [{p}]", "add {t}, {t}, #1", "stlxr {s:w}, {t}, [{p}]", "cbnz {s:w}, 2b",
                            p = in(reg) p + 16, t = out(reg) _, s = out(reg) _, options(nostack));
                        // CAS increment loop.
                        asm!(".arch_extension lse", "ldr {t}, [{p}]", "2:", "add {n}, {t}, #1", "mov {e}, {t}",
                            "casal {e}, {n}, [{p}]", "cmp {e}, {t}", "mov {t}, {e}", "b.ne 2b",
                            p = in(reg) p + 24, t = out(reg) _, n = out(reg) _, e = out(reg) _, options(nostack));
                        // LDUMAX of a value that grows with the thread and the round.
                        asm!(".arch_extension lse", "ldumax {v}, {old}, [{p}]",
                            p = in(reg) p + 32, v = in(reg) t * 1000 + i, old = out(reg) _, options(nostack));
                        // LDSMIN of a negative value, LDSET of a bit per thread, LDEOR twice (back to zero).
                        asm!(".arch_extension lse", "ldsmin {v}, {old}, [{p}]",
                            p = in(reg) p + 40, v = in(reg) (-(t as i64) - 1) as u64, old = out(reg) _, options(nostack));
                        asm!(".arch_extension lse", "ldset {v}, {old}, [{p}]",
                            p = in(reg) p + 48, v = in(reg) 1u64 << t, old = out(reg) _, options(nostack));
                        asm!(".arch_extension lse", "ldeor {v}, {old}, [{p}]", "ldeor {v}, {old}, [{p}]",
                            p = in(reg) p + 56, v = in(reg) 0xffu64 << (8 * t), old = out(reg) _, options(nostack));
                        // SWP of the thread number.
                        asm!(".arch_extension lse", "swp {v}, {old}, [{p}]",
                            p = in(reg) p + 64, v = in(reg) t, old = out(reg) _, options(nostack));
                        // CASP increment loop on a pair of x registers (16-byte aligned).
                        asm!(".arch_extension lse", "ldp x8, x9, [{p}]", "2:", "add x12, x8, #1", "add x13, x9, #2",
                            "mov x10, x8", "mov x11, x9", "caspal x10, x11, x12, x13, [{p}]",
                            "cmp x10, x8", "ccmp x11, x9, #0, eq", "mov x8, x10", "mov x9, x11", "b.ne 2b",
                            p = in(reg) p + 80, out("x8") _, out("x9") _, out("x10") _, out("x11") _,
                            out("x12") _, out("x13") _, options(nostack));
                        // LDXP/STXP increment loop.
                        asm!("2:", "ldaxp x8, x9, [{p}]", "add x8, x8, #1", "add x9, x9, #3",
                            "stlxp {s:w}, x8, x9, [{p}]", "cbnz {s:w}, 2b",
                            p = in(reg) p + 96, s = out(reg) _, out("x8") _, out("x9") _, options(nostack));
                        // LD1/ST1 of two registers, with a post index on the store.
                        let src = p + 128 + 64 * t as usize;
                        asm!("ld1 {{v0.16b, v1.16b}}, [{s}]", "st1 {{v0.16b, v1.16b}}, [{d}], #32",
                            s = in(reg) src, d = inout(reg) src + 32 => _, out("v0") _, out("v1") _, options(nostack));
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let alias = pg.alias_page(1).unwrap();
    let w = |off: usize| u64::from_le_bytes(alias[1024 + off..1024 + off + 8].try_into().unwrap());
    assert_eq!(w(0), 4 * N, "ldadd");
    assert_eq!(w(8) & 0xffff_ffff, 4 * N, "stadd");
    for t in 0..4usize {
        for k in 0..32 {
            let src = 1024 + 128 + 64 * t + k;
            assert_eq!(
                alias[src + 32],
                alias[src],
                "ld1/st1 copy, thread {t} byte {k}"
            );
        }
    }
    assert_eq!(w(16), 4 * N, "ldxr/stxr");
    assert_eq!(w(24), 4 * N, "cas");
    assert_eq!(w(32), 3 * 1000 + N - 1, "ldumax");
    assert_eq!(w(40), (-4i64) as u64, "ldsmin");
    assert_eq!(w(48), 0b1111, "ldset");
    assert_eq!(w(56), 0, "ldeor twice");
    assert!(w(64) < 4, "swp");
    assert_eq!((w(80), w(88)), (4 * N, 8 * N), "casp");
    assert_eq!((w(96), w(104)), (4 * N, 12 * N), "ldxp/stxp");
    let s1 = pg.stats();
    let n = |s: &PageStats, f: Form| s.emulated[f as usize];
    assert_eq!(n(&s1, Form::Rmw) - n(&s0, Form::Rmw), 4 * N * 8);
    assert!(n(&s1, Form::Cas) - n(&s0, Form::Cas) >= 4 * N * 2);
    assert!(n(&s1, Form::Ldxr) - n(&s0, Form::Ldxr) >= 4 * N * 2);
    assert_eq!(n(&s1, Form::Ld1) - n(&s0, Form::Ld1), 4 * N);
    assert_eq!(n(&s1, Form::St1) - n(&s0, Form::St1), 4 * N);
    assert_eq!(s1.fallbacks, s0.fallbacks, "none fell back");
    assert_eq!(s1.writer_waits, s0.writer_waits);
    assert_eq!(pg.state_of(m.addr(1)), "gpu(1)");
    pg.set_mirror(0);
}
