//! Tests of the mirrored mode: copies, waits for the writer, learned readers and eager copies.

use super::*;

#[test]
fn a_new_member_is_copied_into_the_mirror_and_a_gpu_written_page_is_copied_back_on_a_read() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(8);
    m.store(2, 5, 42);
    let mut st = Collect::default();
    let f = pg.flush_mirrored(1, &[(m.addr(2), 8)], &[(m.addr(2), 8)], None, &mut st);
    assert_eq!((f.new_members, f.written, f.staged), (1, 1, 0));
    assert_eq!(mir.load(2, 5), 42, "the guest bytes went into the mirror");
    assert_eq!(pg.state_of(m.addr(2)), "gpu(1)");
    // The GPU writes the mirror; the guest page is stale and has no access.
    mir.store(2, 5, 77);
    mir.store(2, 6, 78);
    // The frame completes; a read copies the page.
    pg.complete(1);
    assert_eq!(
        pg.state_of(m.addr(2)),
        "gpu(1)",
        "the completion leaves the page tagged"
    );
    let s0 = pg.stats();
    assert_eq!(m.load(2, 5), 77);
    assert_eq!(m.load(2, 6), 78);
    assert_eq!(pg.state_of(m.addr(2)), "clean");
    let s1 = pg.stats();
    assert_eq!(s1.synced - s0.synced, 1);
    assert_eq!(
        s1.writer_waits, s0.writer_waits,
        "no wait: the writer was complete"
    );
    // A write now dirties it; the flush stages the whole page and tags it fresh when a frame writes it.
    m.store(2, 7, 9);
    assert_eq!(pg.state_of(m.addr(2)), "dirty");
    let f = pg.flush_mirrored(2, &[], &[(m.addr(2), 8)], None, &mut st);
    assert_eq!((f.staged, f.written), (1, 1));
    assert_eq!(st.pages.len(), 1);
    assert_eq!(
        st.pages[0].1[5], 77,
        "the page holds the GPU's bytes and the CPU's"
    );
    assert_eq!(st.pages[0].1[7], 9);
    assert_eq!(pg.state_of(m.addr(2)), "gpu(2, fresh)");
    pg.complete(2);
    pg.set_mirror(0);
}

#[test]
fn a_read_of_a_page_in_flight_waits_for_the_completion() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(4);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(5, &[], &[(m.addr(1), 1)], None, &mut st);
    mir.store(1, 0, 11);
    let s0 = pg.stats();
    let base = m.base as usize;
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d2 = done.clone();
    let reader = std::thread::spawn(move || {
        // SAFETY: inside the allocation of the test, which outlives the joined thread.
        let v = unsafe { (base as *const u8).add(P as usize).read_volatile() };
        d2.store(true, Ordering::Release);
        v
    });
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(
        !done.load(Ordering::Acquire),
        "the reader waits for the writer"
    );
    // The GPU finishes: it wrote the mirror before the completion.
    mir.store(1, 0, 12);
    pg.complete(5);
    assert_eq!(reader.join().unwrap(), 12);
    let s1 = pg.stats();
    assert_eq!(s1.writer_waits - s0.writer_waits, 1);
    assert!(s1.writer_wait_ns > s0.writer_wait_ns);
    pg.set_mirror(0);
}

#[test]
fn a_reader_goes_on_at_the_pass_signal_of_its_page_while_the_frame_runs_and_the_other_page_waits_for_the_frame()
 {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(6);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(31, &[], &[(m.addr(1), 1), (m.addr(2), 1)], None, &mut st);
    mir.store(1, 0, 11);
    mir.store(2, 0, 21);
    let s0 = pg.stats();
    let base = m.base as usize;
    let read = move |page: usize| {
        std::thread::spawn(move || {
            // SAFETY: inside the allocation of the test, which outlives the joined thread.
            unsafe { (base as *const u8).add(page * P as usize).read_volatile() }
        })
    };
    let (r1, r2) = (read(1), read(2));
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(
        !r1.is_finished() && !r2.is_finished(),
        "both wait for their writers"
    );
    // The pass that writes page 1 is done; the frame is not.
    pg.release_early(31, &[(1, 2)]);
    assert_eq!(r1.join().unwrap(), 11, "the bytes of the pass");
    std::thread::sleep(std::time::Duration::from_millis(40));
    assert!(!r2.is_finished(), "page 2 is written by a later pass");
    assert_eq!(pg.completed(), 0, "the frame did not complete");
    assert_eq!(pg.state_of(m.addr(1)), "clean");
    mir.store(2, 0, 22);
    pg.complete(31);
    assert_eq!(r2.join().unwrap(), 22);
    let s1 = pg.stats();
    assert_eq!(
        s1.wait_ends[0] - s0.wait_ends[0],
        1,
        "one wait ended at a pass signal"
    );
    assert_eq!(
        s1.wait_ends[1] - s0.wait_ends[1],
        1,
        "one at the completion"
    );
    assert_eq!(s1.early_pages - s0.early_pages, 1);
    assert_eq!(s1.early_calls - s0.early_calls, 1);
    pg.set_mirror(0);
}

#[test]
fn a_page_a_reader_waited_for_is_learned_for_a_while_and_forgotten() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(6);
    let mut st = Collect::default();
    let _ = pg.readers(0);
    let _ = pg.flush_mirrored(40, &[], &[(m.addr(3), 1)], None, &mut st);
    mir.store(3, 0, 5);
    let base = m.base as usize;
    let reader = std::thread::spawn(move || {
        // SAFETY: inside the allocation of the test, which outlives the joined thread.
        unsafe { (base as *const u8).add(3 * P as usize).read_volatile() }
    });
    std::thread::sleep(std::time::Duration::from_millis(40));
    pg.complete(40);
    assert_eq!(reader.join().unwrap(), 5);
    let c0 = pg.stats().reader_changes;
    assert_eq!(pg.readers(41), vec![3], "the page waited for the writer");
    assert_eq!(pg.stats().reader_changes - c0, 1, "the set changed once");
    assert_eq!(pg.readers(42), vec![3], "it stays");
    assert_eq!(pg.stats().reader_changes - c0, 1);
    assert!(pg.readers(40 + LEARN_FRAMES + 5).is_empty(), "and decays");
    assert_eq!(pg.stats().reader_changes - c0, 2);
    pg.set_mirror(0);
}

#[test]
fn waits_for_a_writer_are_counted_by_access_with_their_time_and_whether_the_frame_was_committed() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(4);
    let mut st = Collect::default();
    // Frame 21 is flushed and not committed; it may write pages 1 and 2.
    let _ = pg.flush_mirrored(21, &[], &[(m.addr(1), 1), (m.addr(2), 1)], None, &mut st);
    let _ = (pg.take_wait_max(), pg.take_in_flight());
    let s0 = pg.stats();
    let base = m.base as usize;
    let reader = std::thread::spawn(move || {
        // SAFETY: inside the allocation of the test, which outlives the joined thread.
        unsafe { (base as *const u8).add(P as usize).read_volatile() }
    });
    std::thread::sleep(std::time::Duration::from_millis(40));
    // The commit: a wait that begins now begins after it.
    pg.note_committed(21);
    let writer = std::thread::spawn(move || {
        // SAFETY: as above.
        unsafe { (base as *mut u8).add(2 * P as usize).write_volatile(3) };
    });
    std::thread::sleep(std::time::Duration::from_millis(40));
    mir.store(1, 0, 9);
    pg.complete(21);
    assert_eq!(reader.join().unwrap(), 9);
    writer.join().unwrap();
    let s1 = pg.stats();
    assert_eq!(
        (s1.waits[0] - s0.waits[0], s1.waits[1] - s0.waits[1]),
        (1, 1),
        "one read wait, one write wait"
    );
    assert_eq!(
        (
            s1.wait_uncommitted[0] - s0.wait_uncommitted[0],
            s1.wait_uncommitted[1] - s0.wait_uncommitted[1]
        ),
        (1, 0),
        "the read began before the commit, the write after it"
    );
    assert!(
        s1.wait_ns[0] - s0.wait_ns[0] >= 60_000_000,
        "the read waited for both sleeps"
    );
    assert!(s1.wait_ns[1] - s0.wait_ns[1] >= 30_000_000);
    let longest = pg.take_wait_max();
    assert!(longest[0] >= longest[1] && longest[1] >= 30_000_000);
    let (log, total) = pg.take_in_flight();
    assert_eq!(total, 2);
    assert!(
        log.contains(&(21, m.addr(1), false)) && log.contains(&(21, m.addr(2), true)),
        "{log:?}"
    );
    pg.set_mirror(0);
}

#[test]
fn first_writes_after_a_flush_are_logged_for_the_frame_until_its_commit() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, _mir, pg) = mirrored(4);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(31, &[(m.addr(1), 1), (m.addr(3), 1)], &[], None, &mut st);
    m.store(1, 40, 1);
    // A second write to the same page is not a first write.
    m.store(1, 80, 1);
    pg.note_committed(31);
    m.store(3, 0, 1);
    let (log, total) = pg.take_between(31);
    assert_eq!(
        (log, total),
        (vec![m.addr(1) + 40], 1),
        "only the write before the commit, at its address"
    );
    assert_eq!(pg.take_between(31).1, 0, "the log is emptied");
    pg.set_mirror(0);
}

#[test]
fn a_write_in_flight_waits_then_the_cpu_write_lands_on_the_gpus_bytes() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(4);
    let mut st = Collect::default();
    // The frame may write bytes 0 to 127 (a copy from the mirror moves the 64-byte blocks of the written ranges).
    let _ = pg.flush_mirrored(3, &[], &[(m.addr(0), 128)], None, &mut st);
    mir.store(0, 100, 5);
    let s0 = pg.stats();
    let base = m.base as usize;
    let writer = std::thread::spawn(move || {
        // SAFETY: inside the allocation of the test, which outlives the joined thread.
        unsafe { (base as *mut u8).add(101).write_volatile(6) };
    });
    std::thread::sleep(std::time::Duration::from_millis(60));
    mir.store(0, 100, 7);
    pg.complete(3);
    writer.join().unwrap();
    assert_eq!(m.load(0, 100), 7, "the GPU's byte is there");
    assert_eq!(m.load(0, 101), 6, "and the CPU's");
    assert_eq!(pg.state_of(m.addr(0)), "dirty");
    let s1 = pg.stats();
    let d = |k: FaultKind| {
        s1.faults[k as usize].iter().sum::<u64>() - s0.faults[k as usize].iter().sum::<u64>()
    };
    assert_eq!(d(FaultKind::WriteInFlight), 1);
    pg.set_mirror(0);
}

#[test]
fn an_internal_thread_does_not_wait_and_the_page_loses_its_access_at_the_completion() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(4);
    let mut st = Collect::default();
    // A page that the frame writes and that the CPU had not written: the mirror is the truth.
    m.store(1, 0, 1);
    let _ = pg.flush_mirrored(9, &[(m.addr(1), 1)], &[(m.addr(1), 1)], None, &mut st);
    mir.store(1, 0, 2);
    let base = m.base as usize;
    let v = std::thread::spawn(move || {
        let _g = mark_internal_thread();
        // SAFETY: inside the allocation of the test, which outlives the joined thread.
        unsafe { (base as *const u8).add(P as usize).read_volatile() }
    })
    .join()
    .unwrap();
    assert_eq!(v, 2, "the internal thread got the mirror's bytes at once");
    assert_eq!(pg.state_of(m.addr(1)), "open(9)");
    assert!(pg.stats().internal_opens > 0);
    pg.complete(9);
    assert_eq!(pg.state_of(m.addr(1)), "gpu(9)");
    // The GPU wrote more; the game's read after the completion sees it.
    mir.store(1, 0, 3);
    assert_eq!(m.load(1, 0), 3);
    pg.set_mirror(0);
}

#[test]
fn a_poisoned_page_holds_the_pattern_until_a_cpu_access_copies_the_mirror() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(4);
    let mut st = Collect::default();
    m.store(3, 0, 8);
    let _ = pg.flush_mirrored(1, &[], &[(m.addr(3), 1)], Some(0x7fc0_0000), &mut st);
    assert_eq!(mir.load(3, 0), 8);
    pg.complete(1);
    // The access copies the mirror over the pattern.
    assert_eq!(m.load(3, 0), 8);
    pg.set_mirror(0);
}

#[test]
fn a_system_call_reading_a_gpu_written_page_finds_the_mirrors_bytes() {
    use std::io::Write;
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(4);
    nxkit_abi::guest::set_range(m.addr(0), 4 * P);
    let mut st = Collect::default();
    let _ = pg.flush_mirrored(1, &[], &[(m.addr(1), P)], None, &mut st);
    mir.store(1, 0, 0x5a);
    pg.complete(1);
    let out = std::env::temp_dir().join(format!("nxkit-mirror-{}", std::process::id()));
    let mut o = std::fs::File::create(&out).unwrap();
    let w = nxkit_abi::guest::with_slice(m.addr(1), 16, |b| {
        let r = o.write(b);
        (r, b[0])
    });
    let _ = std::fs::remove_file(&out);
    let (r, first) = w.unwrap();
    assert_eq!(r.unwrap(), 16);
    assert_eq!(first, 0x5a);
    nxkit_abi::guest::set_range(0, 0);
    pg.set_mirror(0);
}

#[test]
fn eager_sync_copies_the_pages_back_when_the_frame_completes_and_writes_are_counted_by_phase() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, mir, pg) = mirrored(4);
    let mut st = Collect::default();
    // A member page that the CPU writes while the frame is flushed and not committed (phase 1).
    pg.note_committed(0);
    let _ = pg.flush_mirrored(1, &[(m.addr(2), 1)], &[(m.addr(1), 1)], None, &mut st);
    let s0 = pg.stats();
    m.store(2, 0, 4);
    let s1 = pg.stats();
    assert_eq!(s1.phase_writes[1] - s0.phase_writes[1], 1);
    assert_eq!(
        pg.take_between(1),
        (vec![m.addr(2)], 1),
        "the write is logged for the frame"
    );
    // The late staging takes the page again.
    let late = pg.flush_late(&mut st);
    assert_eq!(late, vec![2]);
    assert_eq!(pg.state_of(m.addr(2)), "clean");
    pg.set_eager_sync(true);
    mir.store(1, 0, 9);
    pg.complete(1);
    assert_eq!(
        pg.state_of(m.addr(1)),
        "clean",
        "copied back without a CPU access"
    );
    assert_eq!(m.load(1, 0), 9);
    pg.set_eager_sync(false);
    pg.note_committed(1);
    pg.set_mirror(0);
}

#[test]
fn set_blocks_sets_the_blocks_a_byte_range_touches() {
    for (lo, hi) in [
        (0u64, 1u64),
        (0, 64),
        (1, 65),
        (63, 64),
        (100, 16384),
        (4000, 4096),
        (0, 16384),
        (4032, 4160),
        (8191, 8193),
    ] {
        let mut got: Mask = [0; 4];
        set_blocks(&mut got, lo, hi);
        let mut want: Mask = [0; 4];
        for b in lo / BLOCK..=(hi - 1) / BLOCK {
            want[(b / 64) as usize] |= 1 << (b % 64);
        }
        assert_eq!(got, want, "{lo}..{hi}");
    }
}

#[test]
fn runs_merge_and_the_lookup_finds_them() {
    assert_eq!(
        runs_of(&[1, 2, 3, 7, 8, 12]),
        vec![(1, 4), (7, 9), (12, 13)]
    );
    let r = vec![(1, 4), (7, 9)];
    assert!(in_runs(&r, 1) && in_runs(&r, 3) && in_runs(&r, 8));
    assert!(!in_runs(&r, 0) && !in_runs(&r, 4) && !in_runs(&r, 9));
}
