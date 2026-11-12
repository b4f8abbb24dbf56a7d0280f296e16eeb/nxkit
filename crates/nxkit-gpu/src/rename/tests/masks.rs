//! Tests of the masks of the CPU and the GPU: merge ranges, probes and the knobs of the judging.

use super::*;

#[test]
fn a_written_range_with_disjoint_words_of_cpu_and_gpu_becomes_a_merge_range() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = never_range();
    for k in 1..=2 {
        run_to(&mut st, &g, k * LEARN_PERIOD - 1);
        st.cands.get_mut(&SUM).unwrap().rest_until = 0;
        judged_rw(&mut st, &g, k * LEARN_PERIOD, 1, 2, false);
        // One judged frame with a CPU write in flight is enough.
        assert_eq!(st.cands[&SUM].learn.class(), Class::Merge);
    }
    // The next flush copies it as a merge range with the learned masks.
    let f = st.flush(
        2 * LEARN_PERIOD + 1,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert_eq!(f.renamed.len(), 1);
    assert_eq!(f.renamed[0].kind, Kind::Merge);
    assert_eq!(f.renamed[0].cmask, vec![0b010]);
    assert_eq!(f.renamed[0].gmask, vec![0b100]);
}

#[test]
fn a_gpu_word_that_went_back_before_the_wait_is_not_a_cpu_word() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = never_range();
    for k in 1..=2 {
        run_to(&mut st, &g, k * LEARN_PERIOD - 1);
        st.cands.get_mut(&SUM).unwrap().rest_until = 0;
        judged_rw(&mut st, &g, k * LEARN_PERIOD, 1, 2, true);
    }
    let c = &st.cands[&SUM];
    assert_eq!(c.cmask, vec![0b010]);
    assert_eq!(c.learn.class(), Class::Merge);
}

#[test]
fn a_range_that_only_the_gpu_writes_is_not_given_up_for_a_quiet_frame_but_settles() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = never_range();
    st.fast_only = true;
    let mut fence = 0;
    // The CPU writes nothing in 29 judged frames: the range stays undecided, judged at every judged flush.
    for _ in 0..SETTLE_PROBES - 1 {
        fence += LEARN_PERIOD;
        run_to(&mut st, &g, fence - 1);
        judged_rw(&mut st, &g, fence, NO_CPU, 2, false);
    }
    assert_eq!(st.cands[&SUM].learn.class(), Class::Never);
    // The key is not taught `Never` yet.
    st.cands.get_mut(&SUM).unwrap().keys.push(key(1, 1, 0x40));
    st.push_keys(SUM);
    assert!(st.keys.is_empty());
    // The 30th quiet frame settles it: it rests, and the key is taught.
    fence += LEARN_PERIOD;
    run_to(&mut st, &g, fence - 1);
    judged_rw(&mut st, &g, fence, NO_CPU, 2, false);
    assert!(st.cands[&SUM].rest_until >= st.frame + REPROBE - 1);
    assert_eq!(st.keys[&key(1, 1, 0x40)].class, Class::Never);
    // A frame in which the CPU does write turns it into a merge range at once, whatever came before.
    fence += (REPROBE.div_ceil(LEARN_PERIOD) + 1) * LEARN_PERIOD;
    run_to(&mut st, &g, fence - 1);
    judged_rw(&mut st, &g, fence, 1, 2, false);
    assert_eq!(st.cands[&SUM].learn.class(), Class::Merge);
    assert_eq!(st.keys[&key(1, 1, 0x40)].class, Class::Merge);
}

#[test]
fn words_that_both_write_are_written_back_by_the_host_and_the_others_by_the_gpu() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = never_range();
    for k in 1..=2 {
        run_to(&mut st, &g, k * LEARN_PERIOD - 1);
        st.cands.get_mut(&SUM).unwrap().rest_until = 0;
        // The CPU and the GPU both write word 1 in this frame.
        judged_rw(&mut st, &g, k * LEARN_PERIOD, 1, 1, false);
    }
    assert_eq!(st.cands[&SUM].learn.class(), Class::Merge);
    // The GPU also keeps state in word 3, which the CPU never writes.
    run_to(&mut st, &g, 3 * LEARN_PERIOD - 1);
    st.cands.get_mut(&SUM).unwrap().rest_until = 0;
    judged_rw(&mut st, &g, 3 * LEARN_PERIOD, 1, 3, false);
    let f = st.flush(
        3 * LEARN_PERIOD + 1,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    let x = &f.renamed[0];
    assert_eq!(x.kind, Kind::Merge);
    assert_eq!(x.cmask, vec![0b0010]);
    assert_eq!(x.gmask, vec![0b1000], "P: the GPU only");
    assert_eq!(x.rmask, vec![0b0010], "R: both");
    // Only the words of the CPU are captured, packed.
    assert_eq!(x.cvals.len(), 1);
    assert!(x.start.is_empty());
}

#[test]
fn a_merge_range_whose_masks_keep_growing_is_a_conflict_that_waits_in_full() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = never_range();
    st.fast_only = true;
    let mut fence = 0;
    // Each judged frame the GPU writes another word.
    for k in 0..12u32 {
        if st.cands[&SUM].learn.class() == Class::Conflict {
            break;
        }
        fence += LEARN_PERIOD;
        run_to(&mut st, &g, fence - 1);
        st.cands.get_mut(&SUM).unwrap().rest_until = 0;
        judged_rw(&mut st, &g, fence, 1, 2 + k % 12, false);
    }
    assert_eq!(st.cands[&SUM].learn.class(), Class::Conflict);
    let f = st.flush(
        fence + 1,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert!(f.conflict && f.renamed.is_empty());
}

#[test]
fn the_start_value_of_a_packed_word_is_found_by_its_index() {
    let r = CapRange {
        addr: 0,
        len: 128,
        start: Vec::new(),
        kind: Kind::Merge,
        cmask: vec![0b1010_0000, 0b1],
        gmask: vec![0, 0],
        rmask: vec![0, 0],
        cvals: vec![11, 22, 33],
        cprefix: vec![0, 2],
    };
    assert_eq!(r.cvalue(5), Some(11));
    assert_eq!(r.cvalue(7), Some(22));
    assert_eq!(r.cvalue(32), Some(33));
    assert_eq!(r.cvalue(6), None);
    assert_eq!(r.cvalue(99), None);
}

#[test]
fn a_probe_that_gets_no_copy_judges_nothing_and_needs_no_full_wait() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = never_range();
    run_to(&mut st, &g, LEARN_PERIOD - 1);
    // A copy overlaps the range: the frame leaves it as it is.
    let f = st.flush(
        LEARN_PERIOD,
        &[(SUM, 0x40)],
        &|r| blocked_flags(r, [(SUM + 4, SUM + 8)]),
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert!(f.renamed.is_empty() && !f.sampling);
    assert!(st.cands[&SUM].sample.is_none());
}

#[test]
fn the_knob_makes_a_frame_with_a_range_that_the_cpu_writes_in_flight_wait_in_full() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = never_range();
    st.cands.get_mut(&SUM).unwrap().hits = SNAPSHOT_HITS;
    let flush = |st: &mut State| {
        st.flush(
            1,
            &[(SUM, 0x40)],
            &|r| vec![false; r.len()],
            &|| Vec::new(),
            &|a, b| g.read(a, b),
        )
    };
    assert!(!flush(&mut st).conflict);
    st.conflict_wait = true;
    assert!(flush(&mut st).conflict);
}

#[test]
fn the_judging_backs_off_when_nothing_new_is_found_and_returns_when_it_is() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = State::default();
    st.candidate(SUM, 0x40);
    assert_eq!(st.period(), LEARN_PERIOD);
    let flush = |st: &mut State| {
        st.flush(
            st.frame + 1,
            &[(SUM, 0x40)],
            &|r| vec![false; r.len()],
            &|| Vec::new(),
            &|a, b| g.read(a, b),
        )
    };
    for _ in 0..QUIET_FRAMES {
        flush(&mut st);
    }
    assert_eq!(st.period(), SLOW_PERIOD);
    // A ring of buffers that comes round is nothing new; an address never seen before is.
    st.candidate(SUM + 0x100, 0x40);
    assert_eq!(st.period(), LEARN_PERIOD);
    for _ in 0..QUIET_FRAMES {
        flush(&mut st);
    }
    assert_eq!(st.period(), SLOW_PERIOD);
    st.cands.remove(&(SUM + 0x100));
    st.candidate(SUM + 0x100, 0x40);
    assert_eq!(st.period(), SLOW_PERIOD);
}

#[test]
fn ranges_are_blocked_by_the_items_that_overlap_them() {
    let ranges = [
        (0x1000, 0x40),
        (0x2000, 0x40),
        (0x2020, 0x40),
        (0x9000, 0x10),
    ];
    let b = blocked_flags(
        &ranges,
        [(0x0f00, 0x1001), (0x2030, 0x2031), (0x5000, 0x6000)],
    );
    assert_eq!(b, vec![true, true, true, false]);
    assert_eq!(blocked_flags(&ranges, []), vec![false; 4]);
}
