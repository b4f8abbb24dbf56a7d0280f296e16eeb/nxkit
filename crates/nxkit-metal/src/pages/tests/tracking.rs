//! Tests of the dirty tracking without a mirror.

use super::*;

#[test]
fn a_write_to_a_clean_page_dirties_it_and_the_flush_cleans_it() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(8);
    let s0 = pg.stats();
    // Page 2 becomes tracked as written by frame 1, completes, and is clean and read only.
    let st = pg.flush(1, &[(m.addr(2), 100)]);
    assert_eq!((st.written, st.new_members), (1, 1));
    pg.complete(1);
    assert_eq!(pg.state_of(m.addr(2)), "clean");
    assert_eq!(m.load(2, 0), 0, "a read of a clean page does not fault");
    m.store(2, 7, 9);
    assert_eq!(m.load(2, 7), 9, "the store ran again after the handler");
    assert_eq!(pg.state_of(m.addr(2)), "dirty");
    // A second store to the open page does not fault.
    m.store(2, 8, 1);
    let s1 = pg.stats();
    assert_eq!(
        s1.faults[FaultKind::CleanWrite as usize]
            .iter()
            .sum::<u64>()
            - s0.faults[FaultKind::CleanWrite as usize]
                .iter()
                .sum::<u64>(),
        1
    );
    // The flush takes it back to clean; the next store faults again.
    let st = pg.flush(2, &[]);
    assert_eq!(st.dirtied, 1);
    assert_eq!(pg.state_of(m.addr(2)), "clean");
    m.store(2, 9, 2);
    assert_eq!(pg.state_of(m.addr(2)), "dirty");
    assert_eq!(m.load(2, 7), 9);
    assert_eq!(m.load(2, 9), 2);
}

#[test]
fn accesses_in_flight_are_counted_by_kind_and_the_data_stays() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(8);
    m.store(3, 0, 5);
    m.store(4, 0, 6);
    let s0 = pg.stats();
    let st = pg.flush(10, &[(m.addr(3), 2 * P)]);
    assert_eq!(st.written, 2);
    assert_eq!(pg.state_of(m.addr(3)), "gpu(10)");
    // A read in flight: counted, the page opens for reading, the data is the guest's.
    assert_eq!(m.load(3, 0), 5);
    // A second read does not fault; a write does, as a write in flight.
    assert_eq!(m.load(3, 1), 0);
    m.store(3, 2, 8);
    // The other page: a write first.
    m.store(4, 3, 7);
    let s1 = pg.stats();
    let d = |k: FaultKind| {
        s1.faults[k as usize].iter().sum::<u64>() - s0.faults[k as usize].iter().sum::<u64>()
    };
    assert_eq!(d(FaultKind::ReadInFlight), 1);
    assert_eq!(d(FaultKind::WriteInFlight), 2);
    assert_eq!(pg.state_of(m.addr(3)), "dirty");
    assert_eq!(m.load(3, 2), 8);
    assert_eq!(m.load(4, 0), 6);
    // The completion leaves dirty pages alone and a later write faults as a plain dirtying only after a flush.
    pg.complete(10);
    assert_eq!(pg.state_of(m.addr(3)), "dirty");
    let _ = pg.flush(11, &[]);
    assert_eq!(pg.state_of(m.addr(3)), "clean");
    let tops = pg.take_top_pcs(FaultKind::WriteInFlight, 10);
    assert!(!tops.is_empty() && tops.iter().map(|t| t.1).sum::<u64>() >= 2);
}

#[test]
fn a_page_read_in_flight_is_clean_after_the_completion() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(4);
    let _ = pg.flush(5, &[(m.addr(1), 1)]);
    assert_eq!(m.load(1, 0), 0);
    assert_eq!(pg.state_of(m.addr(1)), "gpu(5)");
    pg.complete(5);
    assert_eq!(pg.state_of(m.addr(1)), "clean");
    // After the completion the page is protected read only again: a write faults as a clean write.
    let s0 = pg.stats();
    m.store(1, 0, 1);
    let s1 = pg.stats();
    assert_eq!(
        s1.faults[FaultKind::CleanWrite as usize]
            .iter()
            .sum::<u64>()
            - s0.faults[FaultKind::CleanWrite as usize]
                .iter()
                .sum::<u64>(),
        1
    );
}

#[test]
fn a_newer_frame_keeps_the_page_when_the_older_one_completes() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(4);
    let _ = pg.flush(1, &[(m.addr(0), 1)]);
    let _ = pg.flush(2, &[(m.addr(0), 1)]);
    pg.complete(1);
    assert_eq!(pg.state_of(m.addr(0)), "gpu(2)");
    pg.complete(2);
    assert_eq!(pg.state_of(m.addr(0)), "clean");
}

#[test]
fn the_hook_opens_pages_for_system_calls_and_forgets_released_ones() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(6);
    let _ = pg.flush(1, &[(m.addr(0), 3 * P)]);
    pg.complete(1);
    assert_eq!(pg.state_of(m.addr(1)), "clean");
    // A system call writes into pages 1 and 2: both go dirty and writable, page 0 stays clean.
    hook(m.addr(1), 2 * P, nxkit_abi::guest::ACCESS_WRITE);
    assert_eq!(pg.state_of(m.addr(0)), "clean");
    assert_eq!(pg.state_of(m.addr(1)), "dirty");
    assert_eq!(pg.state_of(m.addr(2)), "dirty");
    m.store(1, 0, 1);
    // The host gives the range back: the pages are free and open.
    hook(m.addr(0), 3 * P, nxkit_abi::guest::ACCESS_FORGET);
    assert_eq!(pg.state_of(m.addr(1)), "free");
    assert_eq!(pg.stats().members, 0);
    m.store(0, 0, 1);
}

#[test]
fn a_syscall_read_of_a_page_in_flight_does_not_fail() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(4);
    let _ = pg.flush(1, &[(m.addr(0), 2 * P)]);
    let before = pg.stats().host_access[0];
    hook(m.addr(0), 2 * P, nxkit_abi::guest::ACCESS_READ);
    assert_eq!(pg.stats().host_access[0] - before, 2);
    // The page is readable now, as a system call needs.
    assert_eq!(m.load(0, 0), 0);
    pg.complete(1);
}

#[test]
fn many_threads_write_the_same_clean_page() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(4);
    let _ = pg.flush(1, &[(m.addr(0), P)]);
    pg.complete(1);
    let base = m.base as usize;
    let hs: Vec<_> = (0..8u64)
        .map(|i| {
            std::thread::spawn(move || {
                // SAFETY: inside the allocation of the test, which outlives the joined threads.
                unsafe {
                    (base as *mut u8)
                        .add(i as usize)
                        .write_volatile(i as u8 + 1)
                };
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    for i in 0..8 {
        assert_eq!(m.load(0, i), i as u8 + 1);
    }
    assert_eq!(pg.flush(2, &[]).dirtied, 1);
}

#[test]
fn internal_threads_are_counted_apart() {
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(4);
    let _ = pg.flush(1, &[(m.addr(0), P)]);
    let s0 = pg.stats();
    let base = m.base as usize;
    std::thread::spawn(move || {
        let _g = mark_internal_thread();
        // SAFETY: inside the allocation of the test, which outlives the joined thread.
        unsafe { (base as *mut u8).write_volatile(1) };
    })
    .join()
    .unwrap();
    let s1 = pg.stats();
    assert_eq!(
        s1.faults[FaultKind::WriteInFlight as usize][FaultOrigin::Internal as usize]
            - s0.faults[FaultKind::WriteInFlight as usize][FaultOrigin::Internal as usize],
        1
    );
    pg.complete(1);
}

#[test]
fn a_file_read_into_a_protected_page_works_through_the_guest_slice() {
    use std::io::Read;
    let _t = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let (m, pg) = setup(4);
    nxkit_abi::guest::set_range(m.addr(0), 4 * P);
    let _ = pg.flush(1, &[(m.addr(1), P)]);
    assert_eq!(pg.state_of(m.addr(1)), "gpu(1)");
    let mut f = std::fs::File::open("/dev/zero").unwrap();
    // The page allows no access: the system call would fail with EFAULT without the hook of the slice.
    m.store(1, 5, 0);
    let _ = pg.flush(2, &[(m.addr(1), P)]);
    let r = nxkit_abi::guest::with_slice_mut(m.addr(1), 64, |b| f.read(b).map(|n| (n, b[0])));
    assert_eq!(r.unwrap().unwrap(), (64, 0));
    assert_eq!(pg.state_of(m.addr(1)), "dirty");
    // A read through a system call (a write to a file) opens the page for reading too.
    let _ = pg.flush(3, &[(m.addr(1), P)]);
    let out = std::env::temp_dir().join(format!("nxkit-pages-{}", std::process::id()));
    let mut o = std::fs::File::create(&out).unwrap();
    let w = nxkit_abi::guest::with_slice(m.addr(1), 16, |b| {
        use std::io::Write;
        o.write(b)
    });
    let _ = std::fs::remove_file(&out);
    assert_eq!(w.unwrap().unwrap(), 16);
    nxkit_abi::guest::set_range(0, 0);
    pg.complete(3);
}
