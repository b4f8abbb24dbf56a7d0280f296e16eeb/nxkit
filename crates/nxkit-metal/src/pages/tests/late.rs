//! Tests of results one frame late: snapshots, copies at the completion and system calls.

use super::*;

#[test]
fn a_reader_during_a_frame_gets_the_last_completed_frames_bytes_at_once_and_the_frames_after_it_completes()
 {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    // The frame in flight writes the mirror.
    mir.store(1, 70, 0xB1);
    mir.store(1, 130, 0xB2);
    let s0 = pg.stats();
    let t0 = now_ns();
    assert_eq!(m.load(1, 70), 0xA1, "frame 1's byte, not frame 2's");
    assert_eq!(m.load(1, 130), 0xA2);
    assert!(now_ns() - t0 < 500_000_000, "no wait");
    let s1 = pg.stats();
    assert_eq!(s1.late_reads - s0.late_reads, 2);
    assert_eq!(
        s1.late_age[0] - s0.late_age[0],
        2,
        "the bytes are one frame old"
    );
    assert_eq!(s1.writer_waits, s0.writer_waits);
    assert_eq!(
        s1.fallbacks[Reason::Overlap as usize],
        s0.fallbacks[Reason::Overlap as usize]
    );
    assert_eq!(
        pg.state_of(m.addr(1)),
        "gpu(2)",
        "the page keeps its tag and has no access"
    );
    pg.complete(2);
    assert_eq!(
        m.load(1, 70),
        0xB1,
        "after the completion the reader gets frame 2's bytes"
    );
    assert_eq!(m.load(1, 130), 0xB2);
    done(pg);
}

#[test]
fn the_completion_of_a_frame_copies_its_bytes_for_the_readers_of_the_frame_that_runs_ahead() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let mut c2 = late_start(pg, &mir, 2);
    mir.store(1, 70, 0xB1);
    late_end(&mut c2, &mir);
    // The CPU runs ahead: frame 3 is flushed while frame 2 runs, and tags the page.
    let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let mut c3 = late_start(pg, &mir, 3);
    assert_eq!(
        m.load(1, 70),
        0xA1,
        "frame 3 and 2 run: the bytes are frame 1's"
    );
    // Frame 3 runs on the GPU and writes the mirror before the completion of frame 2 is reported.
    mir.store(1, 70, 0xC0);
    let s0 = pg.stats();
    pg.complete(2);
    let s1 = pg.stats();
    assert_eq!(
        s1.late_copied - s0.late_copied,
        1,
        "the completion copied the page"
    );
    assert_eq!(
        s1.late_exact - s0.late_exact,
        1,
        "from the GPU's copy of the frame's end"
    );
    assert_eq!(
        m.load(1, 70),
        0xB1,
        "frame 3 runs: the bytes are frame 2's, not frame 3's from the mirror"
    );
    assert_eq!(pg.state_of(m.addr(1)), "gpu(3)");
    assert_eq!(pg.stats().writer_waits, s0.writer_waits, "nobody waited");
    mir.store(1, 70, 0xC1);
    late_end(&mut c3, &mir);
    pg.complete(3);
    assert_eq!(m.load(1, 70), 0xC1);
    done(pg);
}

#[test]
fn without_the_gpus_copy_a_page_a_newer_frame_tags_loses_its_snapshot_at_the_completion() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    mir.store(1, 70, 0xB1);
    let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    mir.store(1, 70, 0xC0);
    let s0 = pg.stats();
    pg.complete(2);
    let s1 = pg.stats();
    assert_eq!(
        s1.late_copied, s0.late_copied,
        "the mirror may hold frame 3's bytes: no copy"
    );
    assert_eq!(
        s1.snap_resets[SnapWhy::NoCopy as usize] - s0.snap_resets[SnapWhy::NoCopy as usize],
        1
    );
    mir.store(1, 70, 0xC1);
    pg.complete(3);
    assert_eq!(
        m.load(1, 70),
        0xC1,
        "the read waits for nothing (frame 3 is done) and copies the page"
    );
    done(pg);
}

#[test]
fn a_sum_the_cpu_reads_and_resets_while_the_frame_runs_is_read_next_time_and_the_reset_reaches_the_next_frame()
 {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let mut c2 = late_start(pg, &mir, 2);
    // Frame 2 adds up its sum in the block of byte 70; byte 130 (another block it may write) stays.
    mir.store(1, 70, 0x10);
    // The CPU reads the sum of the last completed frame and resets it for the next frame; it also writes a block
    // the frame does not change.
    assert_eq!(m.load(1, 70), 0xA1);
    m.store(1, 70, 0);
    m.store(1, 130, 0x77);
    late_end(&mut c2, &mir);
    let s0 = pg.stats();
    pg.complete(2);
    let s1 = pg.stats();
    assert_eq!(
        s1.late_frame_won - s0.late_frame_won,
        1,
        "the frame changed the sum: its bytes win"
    );
    assert_eq!(
        s1.late_carried - s0.late_carried,
        1,
        "the reset is kept aside for the next flush"
    );
    assert_eq!(
        m.load(1, 70),
        0x10,
        "the next read of the sum gets frame 2's, not the CPU's reset"
    );
    assert_eq!(
        m.load(1, 130),
        0x77,
        "the block the frame did not change keeps the CPU's byte"
    );
    let mut st3 = Collect::default();
    let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st3);
    let i = st3
        .pages
        .iter()
        .position(|p| p.0 == 1)
        .expect("the page is staged");
    assert_eq!(
        st3.pages[i].1[70], 0,
        "the reset goes into the mirror for frame 3"
    );
    assert_eq!(st3.pages[i].1[130], 0x77);
    assert_eq!(
        st3.skips[i].1[0] & 0b110,
        0,
        "neither block is skipped by the copy-in"
    );
    assert_eq!(pg.stats().late_carry_lost, s1.late_carry_lost);
    pg.complete(3);
    done(pg);
}

#[test]
fn a_reset_staged_before_the_completion_still_lets_the_reader_get_the_frames_sum() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let mut c2 = late_start(pg, &mir, 2);
    mir.store(1, 70, 0x10);
    m.store(1, 70, 0);
    // The flush of frame 3 comes before the completion of frame 2 and stages the reset.
    let mut st3 = Collect::default();
    let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st3);
    let i = st3
        .pages
        .iter()
        .position(|p| p.0 == 1)
        .expect("the page is staged");
    assert_eq!(st3.pages[i].1[70], 0);
    let mut c3 = late_start(pg, &mir, 3);
    late_end(&mut c2, &mir);
    let s0 = pg.stats();
    pg.complete(2);
    let s1 = pg.stats();
    assert_eq!(s1.late_frame_won - s0.late_frame_won, 1);
    assert_eq!(
        s1.late_carried, s0.late_carried,
        "the reset is in the mirror already"
    );
    assert_eq!(
        m.load(1, 70),
        0x10,
        "frame 3 runs: the reader gets frame 2's sum"
    );
    // Frame 3 starts from the reset and adds up its own sum.
    mir.store(1, 70, 0x20);
    late_end(&mut c3, &mir);
    pg.complete(3);
    assert_eq!(m.load(1, 70), 0x20);
    done(pg);
}

#[test]
fn the_completion_of_the_newest_frame_copies_its_bytes_before_the_next_frame_is_flushed() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    mir.store(1, 70, 0xB1);
    // The CPU runs one frame ahead at most: frame 2 completes before frame 3 is flushed, and nothing tags the page.
    let s0 = pg.stats();
    pg.complete(2);
    let s1 = pg.stats();
    assert_eq!(
        s1.late_copied - s0.late_copied,
        1,
        "the completion copied the page of the newest tag"
    );
    assert_eq!(pg.state_of(m.addr(1)), "gpu(2)");
    let _ = pg.flush_mirrored(3, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    assert_eq!(
        m.load(1, 70),
        0xB1,
        "frame 3 runs: the reader gets frame 2's bytes without a wait"
    );
    assert_eq!(pg.stats().writer_waits, s0.writer_waits, "nobody waited");
    pg.complete(3);
    done(pg);
}

#[test]
fn a_snapshot_dropped_at_a_completion_is_counted_with_its_reason_and_the_next_access_names_it() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    // Frame 600 is in another epoch than the read of the page (epoch 0): nobody read the page lately.
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let _ = pg.flush_mirrored(600, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let s0 = pg.stats();
    pg.complete(2);
    let s1 = pg.stats();
    assert_eq!(
        s1.snap_resets[SnapWhy::Idle as usize] - s0.snap_resets[SnapWhy::Idle as usize],
        1
    );
    mir.store(1, 70, 0xB1);
    let base = m.addr(1) as usize;
    let reader = std::thread::spawn(move || {
        // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
        unsafe { ((base + 70) as *const u8).read_volatile() }
    });
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(
        !reader.is_finished(),
        "no snapshot: the reader waits for the writer"
    );
    pg.complete(600);
    assert_eq!(reader.join().unwrap(), 0xB1);
    let s2 = pg.stats();
    assert_eq!(
        s2.snap_misses[SnapWhy::Idle as usize] - s1.snap_misses[SnapWhy::Idle as usize],
        1
    );
    done(pg);
}

#[test]
fn a_page_with_a_snapshot_stays_readable_while_a_frame_writes_it_and_only_stores_fault() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = late_page(0xA1, 0xA2);
    pg.set_read_open(true);
    let mut st = Collect::default();
    let s0 = pg.stats();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    mir.store(1, 70, 0xB1);
    let s1 = pg.stats();
    assert_eq!(
        s1.read_open - s0.read_open,
        1,
        "the flush left the page readable"
    );
    // Reads of the stale blocks and of the other bytes take no trap: nothing is counted by the handler.
    let (emu0, handler0) = (s1.emulated.iter().sum::<u64>(), s1.late_reads);
    assert_eq!(m.load(1, 70), 0xA1, "frame 1's bytes while frame 2 runs");
    assert_eq!(m.load(1, 300), 0);
    let s2 = pg.stats();
    assert_eq!(
        s2.emulated.iter().sum::<u64>(),
        emu0,
        "no emulated access: no fault"
    );
    assert_eq!(s2.late_reads, handler0);
    // A store faults and is done by the handler: the blocks become the CPU's.
    m.store(1, 70, 0xC7);
    let s3 = pg.stats();
    assert_eq!(s3.late_writes - s2.late_writes, 1);
    assert_eq!(s3.writer_waits, s0.writer_waits, "nobody waited");
    pg.complete(2);
    done(pg);
    pg.set_read_open(false);
}

#[test]
fn a_readable_page_nobody_reads_lately_loses_its_access_at_the_completion() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, _mir, pg) = late_page(0xA1, 0xA2);
    pg.set_read_open(true);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    // Frame 600 is in another epoch: when frame 2 completes, the page is not kept current any more.
    let _ = pg.flush_mirrored(600, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let s0 = pg.stats();
    pg.complete(2);
    let s1 = pg.stats();
    assert_eq!(s1.read_revoked - s0.read_revoked, 1);
    let base = m.addr(1) as usize;
    let reader = std::thread::spawn(move || {
        // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
        unsafe { ((base + 70) as *const u8).read_volatile() }
    });
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(
        !reader.is_finished(),
        "the page lost its read access: the reader waits"
    );
    pg.complete(600);
    reader.join().unwrap();
    done(pg);
    pg.set_read_open(false);
}

#[test]
fn a_system_call_read_outside_the_written_bytes_of_a_page_with_a_snapshot_does_not_wait() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, _mir, pg) = late_page(0xA1, 0xA2);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(2, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    let s0 = pg.stats();
    hook(m.addr(1) + 300, 8, nxkit_abi::guest::ACCESS_READ);
    let s1 = pg.stats();
    assert_eq!(s1.settle_late - s0.settle_late, 1);
    assert_eq!(s1.settle_waits, s0.settle_waits);
    assert_eq!(
        m.load(1, 300),
        0,
        "the page is readable for the system call"
    );
    pg.complete(2);
    done(pg);
}

#[test]
fn a_page_that_nobody_waited_for_still_waits() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = emulating(4);
    pg.set_late(true);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(1, &[], &[(m.addr(1) + 64, 128)], None, &mut st);
    mir.store(1, 70, 5);
    let s0 = pg.stats();
    let base = m.addr(1) as usize;
    let reader = std::thread::spawn(move || {
        // SAFETY: inside page 1 of the test mapping, which outlives the joined thread.
        unsafe { ((base + 70) as *const u8).read_volatile() }
    });
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(
        !reader.is_finished(),
        "the guest bytes are not known to be a completed frame's: it waits"
    );
    pg.complete(1);
    assert_eq!(reader.join().unwrap(), 5);
    let s1 = pg.stats();
    assert_eq!(s1.late_reads, s0.late_reads);
    assert_eq!(s1.writer_waits - s0.writer_waits, 1);
    done(pg);
}
