//! Tests of results one frame late: CPU writes while a frame runs.

use super::*;

#[test]
fn a_cpu_write_during_a_frame_survives_the_copy_back_of_the_frame_and_is_staged_at_the_next_flush()
{
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    pg.set_adaptive(true, false);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    mir.store(1, 70, 0xB1);
    mir.store(1, 130, 0xB2);
    let s0 = pg.stats();
    m.store(1, 70, 0x55);
    let s1 = pg.stats();
    assert_eq!(s1.late_writes - s0.late_writes, 1);
    assert_eq!(s1.late_owned - s0.late_owned, 1, "one 64-byte block");
    assert_eq!(s1.writer_waits, s0.writer_waits);
    assert_eq!(m.load(1, 70), 0x55, "the CPU reads what it wrote");
    pg.complete(2);
    wait_clean(pg, &m);
    assert_eq!(
        m.load(1, 70),
        0x55,
        "the copy-back of frame 2 left the block the CPU wrote"
    );
    assert_eq!(m.load(1, 130), 0xB2, "and copied the other block");
    assert!(pg.stats().late_kept > s1.late_kept);
    // The next flush stages the page with the CPU's byte.
    let mut st3 = Collect::default();
    let f = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st3);
    assert_eq!(f.staged, 1);
    let i = st3
        .pages
        .iter()
        .position(|p| p.0 == 1)
        .expect("the page is staged");
    assert_eq!(st3.pages[i].1[70], 0x55);
    assert_eq!(
        st3.skips[i].1, [0; 4],
        "the page is current in guest memory: nothing is skipped"
    );
    pg.complete(3);
    pg.set_adaptive(false, false);
    done(pg);
}

#[test]
fn a_write_staged_while_its_frame_runs_skips_the_blocks_only_the_frame_wrote_and_survives_the_frames_completion()
 {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let mut c2 = late_start(pg, &mir, 2);
    // Frame 2 writes block 2 only; the CPU writes block 1.
    mir.store(1, 130, 0xB2);
    m.store(1, 70, 0x55);
    // The CPU runs ahead: the flush of frame 3 stages the page while frame 2 runs.
    let mut st3 = Collect::default();
    let f = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st3);
    let mut c3 = late_start(pg, &mir, 3);
    late_end(&mut c2, &mir);
    assert_eq!(f.emu_staged, 1);
    let i = st3
        .pages
        .iter()
        .position(|p| p.0 == 1)
        .expect("the page is staged");
    assert_eq!(st3.pages[i].1[70], 0x55);
    // Block 1 (bytes 64 to 127) is the CPU's and goes along; block 2 (128 to 191) is the frame's and is skipped.
    assert_eq!(
        st3.skips[i].1[0], 0b100,
        "only the block of the frame is skipped"
    );
    // The frame completes while the page is tagged by frame 3: its copy-back keeps the staged block.
    pg.complete(2);
    assert_eq!(
        m.load(1, 70),
        0x55,
        "the copy-back of frame 2 does not take back what the CPU wrote before frame 3"
    );
    assert_eq!(m.load(1, 130), 0xB2, "the other block is frame 2's");
    // The staged block is not staged again at the next flush: a frame may have written it since.
    let mut st4 = Collect::default();
    let _ = pg.flush_mirrored(4, &[], &[(m.addr(1) + 64, 128)], None, &mut st4);
    assert!(st4.pages.iter().all(|p| p.0 != 1), "nothing new to stage");
    late_end(&mut c3, &mir);
    pg.complete(3);
    pg.complete(4);
    done(pg);
}

#[test]
fn an_atomic_add_during_a_frame_lands_on_guest_memory_and_is_staged() {
    use std::arch::asm;
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = emulating(4);
    pg.set_late(true);
    let mut st = Collect::default();
    // Frame 1's result: a counter of 10 at byte 64.
    let _ = pg.flush_mirrored(1, &[], &[(m.addr(1) + 64, 64)], None, &mut st);
    for (k, b) in 10u32.to_le_bytes().iter().enumerate() {
        mir.store(1, 64 + k as u64, *b);
    }
    pg.complete(1);
    assert_eq!(m.load(1, 64), 10);
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 64)], None, &mut st);
    let mut c2 = late_start(pg, &mir, 2);
    for (k, b) in 99u32.to_le_bytes().iter().enumerate() {
        mir.store(1, 64 + k as u64, *b);
    }
    let s0 = pg.stats();
    let p = m.addr(1) as usize + 64;
    let old: u32;
    // SAFETY: inside page 1 of the test mapping, aligned for a word.
    unsafe {
        asm!(".arch_extension lse", "ldaddal {one:w}, {old:w}, [{p}]",
            p = in(reg) p, one = in(reg) 5u32, old = out(reg) old, options(nostack));
    }
    assert_eq!(
        old, 10,
        "the atomic operates on the last completed frame's value"
    );
    let s1 = pg.stats();
    assert_eq!(s1.late_writes - s0.late_writes, 1);
    assert_eq!(s1.writer_waits, s0.writer_waits);
    let mut st3 = Collect::default();
    let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 64)], None, &mut st3);
    let i = st3
        .pages
        .iter()
        .position(|p| p.0 == 1)
        .expect("the page is staged");
    assert_eq!(
        u32::from_le_bytes(st3.pages[i].1[64..68].try_into().unwrap()),
        15,
        "the sum is staged"
    );
    assert_eq!(st3.skips[i].1[0] & 0b10, 0, "the block is not skipped");
    late_end(&mut c2, &mir);
    pg.complete(2);
    // Frame 2 changed the counter: its value is the result the reader gets one frame late, and the CPU's sum is in
    // the mirror for frame 3.
    let read = u32::from_le_bytes([m.load(1, 64), m.load(1, 65), m.load(1, 66), m.load(1, 67)]);
    assert_eq!(read, 99, "the frame's 99 wins for the reader");
    pg.complete(3);
    done(pg);
}

#[test]
fn with_late_results_off_an_access_to_written_bytes_waits() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    pg.set_late(false);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    mir.store(1, 70, 0xB1);
    let s0 = pg.stats();
    let base = m.addr(1) as usize;
    let reader = std::thread::spawn(move || {
        // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
        unsafe { ((base + 70) as *const u8).read_volatile() }
    });
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(!reader.is_finished());
    pg.complete(2);
    assert_eq!(reader.join().unwrap(), 0xB1);
    assert_eq!(pg.stats().late_reads, s0.late_reads);
    done(pg);
}
