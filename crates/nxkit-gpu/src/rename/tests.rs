use super::*;
use std::cell::RefCell;

fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn a_frame_in_which_the_gpu_wrote_nothing_is_quiet() {
    let a = words(&[1, 2, 3]);
    assert_eq!(judge(&a, &a, &words(&[9, 9, 9])), Verdict::Quiet);
}

#[test]
fn a_word_that_the_cpu_changed_again_is_clean() {
    // The GPU summed into word 0, the CPU read it and reset it.
    let pre = words(&[0, 5]);
    let post = words(&[7, 5]);
    let next = words(&[0, 5]);
    assert_eq!(judge(&pre, &post, &next), Verdict::Clean);
}

#[test]
fn a_word_that_the_cpu_left_alone_is_dirty() {
    let pre = words(&[0, 5]);
    let post = words(&[7, 5]);
    assert_eq!(judge(&pre, &post, &post), Verdict::Dirty);
    // One word re-initialised and one kept is dirty too.
    let post = words(&[7, 8]);
    let next = words(&[0, 8]);
    assert_eq!(judge(&pre, &post, &next), Verdict::Dirty);
}

#[test]
fn only_the_words_that_the_gpu_wrote_must_be_reset() {
    // Word 1 is CPU data that does not change; the GPU did not write it.
    let pre = words(&[0, 5, 0]);
    let post = words(&[3, 5, 4]);
    let next = words(&[0, 5, 0]);
    assert_eq!(judge(&pre, &post, &next), Verdict::Clean);
}

#[test]
fn a_tail_shorter_than_a_word_counts() {
    assert_eq!(
        judge(&[1, 2, 3, 4, 5], &[1, 2, 3, 4, 6], &[1, 2, 3, 4, 5]),
        Verdict::Clean
    );
    assert_eq!(
        judge(&[1, 2, 3, 4, 5], &[1, 2, 3, 4, 6], &[1, 2, 3, 4, 6]),
        Verdict::Dirty
    );
}

fn frame(l: &mut Learn, v: Verdict) -> Option<Class> {
    l.tick();
    l.observe(v)
}

#[test]
fn a_range_is_renamed_after_the_clean_frames() {
    let mut l = Learn::new();
    for _ in 0..LEARN_FRAMES - 1 {
        assert_eq!(frame(&mut l, Verdict::Clean), None);
        assert!(l.sampling() && !l.renaming());
    }
    assert_eq!(frame(&mut l, Verdict::Clean), Some(Class::Rename));
    assert!(l.renaming() && !l.sampling());
    assert_eq!(l.frames(), u64::from(LEARN_FRAMES));
}

#[test]
fn quiet_frames_neither_count_nor_reset() {
    let mut l = Learn::new();
    for _ in 0..LEARN_FRAMES - 1 {
        frame(&mut l, Verdict::Clean);
        for _ in 0..3 {
            assert_eq!(frame(&mut l, Verdict::Quiet), None);
        }
    }
    assert_eq!(l.class(), Class::Learning);
    assert_eq!(frame(&mut l, Verdict::Clean), Some(Class::Rename));
}

#[test]
fn one_dirty_frame_makes_a_learning_range_never() {
    let mut l = Learn::new();
    for _ in 0..LEARN_FRAMES - 1 {
        frame(&mut l, Verdict::Clean);
    }
    assert_eq!(frame(&mut l, Verdict::Dirty), Some(Class::Never));
    assert!(!l.sampling() && !l.renaming());
    // Nothing brings it back.
    for _ in 0..1000 {
        assert_eq!(frame(&mut l, Verdict::Clean), None);
    }
    assert_eq!(l.class(), Class::Never);
}

#[test]
fn a_range_that_stays_quiet_rests_and_learns_again() {
    let mut l = Learn::new();
    for _ in 0..QUIET_LIMIT - 1 {
        assert_eq!(frame(&mut l, Verdict::Quiet), None);
    }
    assert_eq!(frame(&mut l, Verdict::Quiet), Some(Class::Idle));
    assert!(!l.sampling());
    for _ in 0..RECHECK_EVERY - 1 {
        l.tick();
        assert_eq!(l.class(), Class::Idle);
    }
    l.tick();
    assert_eq!(l.class(), Class::Learning);
    assert!(l.sampling());
}

fn renamed() -> Learn {
    let mut l = Learn::new();
    for _ in 0..LEARN_FRAMES {
        frame(&mut l, Verdict::Clean);
    }
    assert_eq!(l.class(), Class::Rename);
    l
}

#[test]
fn a_renamed_range_is_checked_every_so_many_frames() {
    let mut l = renamed();
    for _ in 0..RECHECK_EVERY - 1 {
        l.tick();
        assert!(l.renaming());
    }
    l.tick();
    // The check frame is bound to the guest range.
    assert!(l.sampling() && !l.renaming());
    assert_eq!(l.observe(Verdict::Clean), None);
    assert!(l.renaming());
    // The next check comes after another period.
    for _ in 0..RECHECK_EVERY - 1 {
        l.tick();
    }
    assert!(l.renaming());
    l.tick();
    assert!(l.sampling());
}

#[test]
fn a_dirty_check_demotes_a_renamed_range() {
    let mut l = renamed();
    for _ in 0..RECHECK_EVERY {
        l.tick();
    }
    assert_eq!(l.observe(Verdict::Dirty), Some(Class::Never));
    assert!(!l.renaming() && !l.sampling());
}

#[test]
fn a_check_that_hears_nothing_ends_after_the_quiet_limit() {
    let mut l = renamed();
    for _ in 0..RECHECK_EVERY {
        l.tick();
    }
    for _ in 0..QUIET_LIMIT - 1 {
        assert_eq!(l.observe(Verdict::Quiet), None);
        assert!(l.sampling());
    }
    assert_eq!(l.observe(Verdict::Quiet), None);
    assert!(l.renaming());
}

#[test]
fn a_verdict_outside_a_check_does_not_touch_a_renamed_range() {
    let mut l = renamed();
    assert_eq!(l.observe(Verdict::Dirty), None);
    assert!(l.renaming());
}

#[test]
fn a_range_is_renamed_unless_something_else_touches_it() {
    let cands = [
        (0x1000, 0x40),
        (0x2000, 0x40),
        (0x3000, 0x40),
        (0x3020, 0x40),
    ];
    // The binding of the first reaches out of its range; the second has a copy over it; the last two overlap.
    let bound = [
        (0x0ff0, 0x40),
        (0x2000, 0x40),
        (0x3000, 0x40),
        (0x3020, 0x40),
    ];
    let others = [(0x2010, 0x2014)];
    let (ok, left) = plan(
        &cands,
        &bound,
        &blocked_flags(&cands, others.iter().copied()),
    );
    // 0x3000..0x3040 holds a binding that starts at 0x3020 and reaches out of it, so both are left.
    assert_eq!(ok, Vec::<usize>::new());
    assert_eq!(left, 4);
    let (ok, left) = plan(&cands[2..3], &bound[2..3], &[]);
    assert_eq!((ok, left), (vec![0], 0));
}

#[test]
fn a_binding_inside_a_range_does_not_exclude_it() {
    let cands = [(0x1000, 0x40)];
    let bound = [(0x1000, 0x40), (0x1010, 0x10), (0x1030, 0x10)];
    assert_eq!(
        plan(&cands, &bound, &blocked_flags(&cands, [(0x1040, 0x1050)])),
        (vec![0], 0)
    );
}

#[test]
fn two_ranges_that_overlap_are_not_both_renamed() {
    let cands = [(0x1000, 0x40), (0x1020, 0x40)];
    let (ok, left) = plan(&cands, &[], &[]);
    assert_eq!((ok, left), (vec![0], 1));
}

#[test]
fn the_map_gives_the_copy_with_the_same_offset() {
    let m = RenameMap::from_entries(vec![(0x2000, 0xc0, 0x9040), (0x1000, 0x40, 0x9000)]);
    assert_eq!(m.resolve(0x1000, 0x40), Some(0x9000));
    assert_eq!(m.resolve(0x1010, 8), Some(0x9010));
    assert_eq!(m.resolve(0x2030, 0x10), Some(0x9070));
    // Reaches out, lies outside, or is before the first range.
    assert_eq!(m.resolve(0x1030, 0x20), None);
    assert_eq!(m.resolve(0x1040, 4), None);
    assert_eq!(m.resolve(0x500, 4), None);
    assert_eq!(RenameMap::new().resolve(0x1000, 4), None);
}

#[test]
fn a_word_goes_back_only_where_the_cpu_left_it() {
    // The guest word still holds the start: the end goes in.
    assert_eq!(decide_word(0, 0, 7, None, false), Some(7));
    // The CPU wrote it since the flush.
    assert_eq!(decide_word(1, 0, 7, None, false), None);
}

#[test]
fn two_frames_without_a_wait_between_them_add_up() {
    let f = |v: f32| v.to_bits();
    // The write-back of the frame before wrote 25.0; this frame added 24.0 onto 0.0.
    let new = decide_word(f(25.0), f(0.0), f(24.0), Some(f(25.0)), true).unwrap();
    assert_eq!(f32::from_bits(new), 49.0);
}

#[test]
fn a_wait_between_two_frames_replaces_the_older_value() {
    assert_eq!(decide_word(5, 0, 9, Some(5), false), Some(9));
    // A word that the CPU changed after the write-back stays.
    assert_eq!(decide_word(6, 0, 9, Some(5), false), None);
    assert_eq!(decide_word(6, 0, 9, Some(5), true), None);
}

/// A guest memory for the state machine: bytes by address.
struct Guest(RefCell<Vec<u8>>);

impl Guest {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let m = self.0.borrow();
        let a = (addr - BASE) as usize;
        match m.get(a..a + buf.len()) {
            Some(s) => {
                buf.copy_from_slice(s);
                true
            }
            None => false,
        }
    }

    fn set(&self, addr: u64, w: &[u32]) {
        let mut m = self.0.borrow_mut();
        let a = (addr - BASE) as usize;
        m[a..a + 4 * w.len()].copy_from_slice(&words(w));
    }
}

const BASE: u64 = 0x1000;
const SUM: u64 = BASE + 0x100;

fn state_with_sum() -> State {
    let mut st = State::default();
    st.candidate(SUM, 0x40);
    st
}

/// One game frame as the host sees it: the CPU resets the word, the frame is flushed, the GPU adds `sum` into it and the
/// wait reads the bytes after the frame.
fn play(st: &mut State, g: &Guest, fence: u64, sum: u32, reset: bool) -> Flushed {
    if reset {
        g.set(SUM, &[0]);
    }
    let bound = [(SUM, 0x40)];
    let f = st.flush(
        fence,
        &bound,
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    // The GPU adds into the guest word when the frame is not renamed.
    if f.renamed.is_empty() {
        let mut w = [0u8; 4];
        g.read(SUM, &mut w);
        g.set(SUM, &[u32::from_le_bytes(w) + sum]);
    }
    st.waited(fence, fence, &|a, b| g.read(a, b));
    f
}

#[test]
fn a_sum_that_the_cpu_resets_is_learned_and_then_renamed() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = state_with_sum();
    let mut renamed_at = None;
    for fence in 1..=300u64 {
        let f = play(&mut st, &g, fence, 3, true);
        if !f.renamed.is_empty() {
            renamed_at.get_or_insert(fence);
            assert_eq!(f.renamed[0].addr, SUM);
            assert_eq!(f.renamed[0].start.len(), 0x40);
            // The frame is renamed: the capture holds the reset word.
            assert_eq!(&f.renamed[0].start[..4], &[0, 0, 0, 0]);
        }
    }
    // Only one frame in LEARN_PERIOD is judged, at the flush after it: the flush after the eighth clean frame already
    // reads a copy.
    assert_eq!(renamed_at, Some(LEARN_PERIOD * u64::from(LEARN_FRAMES) + 1));
    assert_eq!(st.cands[&SUM].learn.class(), Class::Rename);
}

#[test]
fn a_flush_that_overtakes_the_wait_is_not_judged() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = state_with_sum();
    // The CPU flushes the next frame before the wait for the one before: `post` is not read in time.
    for fence in 1..=200u64 {
        g.set(SUM, &[0]);
        let f = st.flush(
            fence,
            &[(SUM, 0x40)],
            &|r| vec![false; r.len()],
            &|| Vec::new(),
            &|a, b| g.read(a, b),
        );
        assert!(f.renamed.is_empty());
        g.set(SUM, &[5]);
        // The wait for the frame before the newest: the newest flush has not finished.
        st.waited(fence - 1, fence, &|a, b| g.read(a, b));
    }
    assert_eq!(st.cands[&SUM].learn.class(), Class::Learning);
}

#[test]
fn state_that_the_gpu_keeps_is_never_renamed() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = state_with_sum();
    for fence in 1..=200u64 {
        // The CPU does not reset the word: the GPU keeps adding.
        let f = play(&mut st, &g, fence, 3, false);
        // A judged frame may give the range a probe copy; nothing else is renamed.
        assert!(f.renamed.iter().all(|r| r.kind == Kind::Probe));
    }
    assert_eq!(st.cands[&SUM].learn.class(), Class::Never);
}

#[test]
fn a_renamed_range_that_turns_into_state_is_demoted_by_the_check() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = state_with_sum();
    st.fast_only = true;
    let mut fence = 0;
    while st.cands[&SUM].learn.class() != Class::Rename {
        fence += 1;
        play(&mut st, &g, fence, 3, true);
    }
    // Renamed frames: the host copies and writes back; here the range is only counted until the check is due.
    let mut checked = false;
    for _ in 0..RECHECK_EVERY + 40 {
        fence += 1;
        // From now on the CPU keeps what the GPU wrote.
        let f = play(&mut st, &g, fence, 3, false);
        if st.cands[&SUM].learn.sampling() {
            checked = true;
        }
        if f.sampling && st.cands[&SUM].learn.class() == Class::Never {
            break;
        }
    }
    assert!(checked);
    assert_eq!(st.cands[&SUM].learn.class(), Class::Never);
}

#[test]
fn a_range_that_no_frame_binds_is_forgotten_in_time() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = state_with_sum();
    for fence in 1..=3600u64 {
        st.flush(
            fence,
            &[],
            &|r| vec![false; r.len()],
            &|| Vec::new(),
            &|a, b| g.read(a, b),
        );
    }
    assert!(st.cands.is_empty());
}

#[test]
fn a_binding_larger_than_the_range_grows_it_and_starts_again() {
    let mut st = state_with_sum();
    st.cands.get_mut(&SUM).unwrap().learn = renamed();
    st.candidate(SUM, 0x80);
    assert_eq!(st.cands[&SUM].size, 0x80);
    assert_eq!(st.cands[&SUM].learn.class(), Class::Learning);
}

#[test]
fn a_watched_range_reports_what_the_cpu_wrote_while_its_frame_ran() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = State {
        watch: true,
        ..State::default()
    };
    st.watch_read_only(SUM, 0x40);
    // The frame that is judged: the 16th flush.
    for fence in 1..LEARN_PERIOD {
        st.flush(
            fence,
            &[(SUM, 0x40)],
            &|r| vec![false; r.len()],
            &|| Vec::new(),
            &|a, b| g.read(a, b),
        );
    }
    let fence = LEARN_PERIOD;
    let f = st.flush(
        fence,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    // A read-only range is measured when the wait begins and needs no full wait.
    assert!(f.watching && !f.sampling);
    // The wait begins, the CPU has written word 1 meanwhile.
    g.set(SUM + 4, &[7]);
    st.before_wait(fence, &|a, b| g.read(a, b));
    let e = &st.inflight[&SUM];
    assert_eq!((e.frames, e.hit, e.words), (1, 1, 1));
    assert!(e.ro);
}

/// One judged frame of a read-only range: the flush, the CPU writing a word in flight, the wait.
fn judged_frame(st: &mut State, g: &Guest, fence: u64, write: bool) -> Flushed {
    let f = st.flush(
        fence,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    if write {
        g.set(SUM + 4, &[fence as u32]);
    }
    st.before_wait(fence, &|a, b| g.read(a, b));
    st.waited(fence, fence, &|a, b| g.read(a, b));
    f
}

fn run_to(st: &mut State, g: &Guest, last: u64) {
    for fence in st.frame + 1..=last {
        st.flush(
            fence,
            &[(SUM, 0x40)],
            &|r| vec![false; r.len()],
            &|| Vec::new(),
            &|a, b| g.read(a, b),
        );
    }
}

#[test]
fn a_read_only_range_that_the_cpu_writes_in_flight_becomes_a_snapshot() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = State::default();
    st.watch_read_only(SUM, 0x40);
    run_to(&mut st, &g, LEARN_PERIOD - 1);
    judged_frame(&mut st, &g, LEARN_PERIOD, true);
    // One judged frame with a CPU write in flight is enough.
    assert_eq!(st.cands[&SUM].learn.class(), Class::Snapshot);
    // The next flush announces it and copies the range.
    let f = st.flush(
        LEARN_PERIOD + 1,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert!(f.news.iter().any(|n| n.2 == Class::Snapshot));
    assert_eq!(f.renamed.len(), 1);
    assert_eq!(f.renamed[0].kind, Kind::Snapshot);
    assert_eq!(f.snapshot_bytes, 0x40);
}

#[test]
fn a_read_only_range_that_the_cpu_leaves_alone_is_watched_and_then_rests() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = State {
        fast_only: true,
        ..State::default()
    };
    st.watch_read_only(SUM, 0x40);
    let mut fence = 0;
    // A range that the CPU writes only in some frames is not given up for one quiet frame.
    for _ in 0..SETTLE_PROBES {
        fence += LEARN_PERIOD;
        run_to(&mut st, &g, fence - 1);
        let f = judged_frame(&mut st, &g, fence, false);
        // A read-only range needs no full wait.
        assert!(f.watching && !f.sampling);
    }
    // After that it rests.
    fence += LEARN_PERIOD;
    run_to(&mut st, &g, fence - 1);
    let f = judged_frame(&mut st, &g, fence, false);
    assert!(!f.watching);
    assert_eq!(st.cands[&SUM].learn.class(), Class::Watch);
}

#[test]
fn a_snapshot_that_a_shader_writes_is_never() {
    let mut st = State::default();
    st.watch_read_only(SUM, 0x40);
    st.cands.get_mut(&SUM).unwrap().learn.set(Class::Snapshot);
    assert!(st.is_snapshot(SUM));
    st.candidate(SUM, 0x40);
    assert_eq!(st.cands[&SUM].learn.class(), Class::Never);
    assert!(!st.is_snapshot(SUM));
}

#[test]
fn a_snapshot_that_a_copy_writes_is_never() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = State::default();
    st.watch_read_only(SUM, 0x40);
    st.cands.get_mut(&SUM).unwrap().learn.set(Class::Snapshot);
    let f = st.flush(
        1,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| vec![(SUM + 8, SUM + 16)],
        &|a, b| g.read(a, b),
    );
    assert!(f.renamed.is_empty());
    assert_eq!(st.cands[&SUM].learn.class(), Class::Never);
}

#[test]
fn snapshots_over_the_budget_of_a_frame_are_left_out() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = State::default();
    let big = (SNAPSHOT_BUDGET / 2 + 1) as u32;
    // Reads past the small guest fail; the state only needs the sizes, so the reader says yes.
    let read = |_: u64, b: &mut [u8]| {
        b.fill(0);
        true
    };
    for k in 0..2u64 {
        let a = 0x1000_0000 * (k + 1);
        st.watch_read_only(a, big);
        st.cands.get_mut(&a).unwrap().learn.set(Class::Snapshot);
    }
    let _ = &g;
    let bound = [(0x1000_0000, big), (0x2000_0000, big)];
    let f = st.flush(1, &bound, &|r| vec![false; r.len()], &|| Vec::new(), &read);
    assert_eq!(f.renamed.len(), 1);
    assert_eq!(f.over_budget, 1);
    assert_eq!(f.snapshot_bytes, u64::from(big));
}

#[test]
fn the_masks_of_the_cpu_and_the_gpu_are_found_from_the_three_copies() {
    // Word 0: the GPU wrote it after the wait began. Word 1: the CPU wrote it in flight. Word 2: nobody.
    let pre = words(&[0, 1, 2]);
    let atwait = words(&[0, 5, 2]);
    assert_eq!(cpu_mask(&pre, &atwait), vec![0b010]);
    assert_eq!(
        gpu_words(&words(&[9, 5, 2]), &words(&[0, 5, 2])),
        vec![0b001]
    );
    let mut m = vec![0b100];
    assert!(union_mask(&mut m, &[0b001, 0b1]));
    assert_eq!(m, vec![0b101, 1]);
    assert!(!union_mask(&mut m, &[0b100]));
    assert!(is_empty_mask(&[0, 0]) && !is_empty_mask(&[0, 2]));
}

/// `cpu_word` of a judged frame in which the CPU writes nothing.
const NO_CPU: u32 = u32::MAX;

/// A judged frame of a written range, as the host sees it: the flush gives the range a copy (a probe); the CPU writes
/// `cpu_word` after it; the wait begins (`landed` says whether the GPU's word has already gone back into guest memory);
/// then the GPU is done with a copy that has `gpu_word` changed.
fn judged_rw(
    st: &mut State,
    g: &Guest,
    fence: u64,
    cpu_word: u32,
    gpu_word: u32,
    landed: bool,
) -> Flushed {
    let f = st.flush(
        fence,
        &[(SUM, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert_eq!(
        f.renamed.len(),
        1,
        "the range gets a copy in the judged frame"
    );
    assert_eq!(f.renamed[0].kind, Kind::Probe);
    assert!(
        f.renamed[0].cmask.iter().all(|w| *w == 0),
        "no CPU word yet"
    );
    assert!(f.sampling);
    let mut start = vec![0u8; f.renamed[0].len as usize];
    g.read(SUM, &mut start);
    let mut end = start.clone();
    let at = 4 * gpu_word as usize;
    end[at..at + 4].copy_from_slice(&(fence as u32 + 1000).to_le_bytes());
    if cpu_word != NO_CPU {
        g.set(SUM + 4 * u64::from(cpu_word), &[fence as u32]);
    }
    if landed {
        g.set(SUM + 4 * u64::from(gpu_word), &[fence as u32 + 1000]);
    }
    st.before_wait(fence, &|a, b| g.read(a, b));
    st.probed(fence, SUM, &end, &start);
    f
}

fn never_range() -> State {
    let mut st = State::default();
    st.candidate(SUM, 0x40);
    st.cands.get_mut(&SUM).unwrap().learn.set(Class::Never);
    st
}

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

/// The cost of a frame of the renamer on the CPU thread, for a frame of 3000 draws with 4 storage buffers, 4 uniform buffers
/// and 2 vertex streams each, 650 candidate ranges of which 24 are renamed. Run with
/// `cargo test --release -p nxkit-gpu --lib renamer_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn renamer_cost_of_a_big_frame() {
    use crate::detail::{Stage, StreamDetail, UboDetail};
    let Ok(ctx) = nxkit_metal::Context::system_default() else {
        return;
    };
    let mem = Arc::new(nxkit_metal::GuestMemory::allocate(&ctx, 1 << 28).unwrap());
    let guest = nxkit_metal::GuestSpace::over(&mem).unwrap();
    let base = guest.address_map().gpu_base();
    let renamer = Renamer::new(&ctx, guest);
    let keys: Vec<Arc<str>> = (0..60).map(|i| Arc::from(format!("k{i}"))).collect();
    let mut detail = FrameDetail::default();
    for n in 0..3000u64 {
        let mut d = crate::walk::empty_draw();
        d.program.vertex = keys[(n % 60) as usize].clone();
        d.program.fragment = Some(keys[((n + 7) % 60) as usize].clone());
        for slot in 0..4u32 {
            // 650 ranges of 16 KiB to 40 KiB, spread over the memory.
            let i = (n * 4 + u64::from(slot)) % 650;
            d.sbufs.push(UboDetail {
                stage: if slot < 2 {
                    Stage::Vertex
                } else {
                    Stage::Fragment
                },
                slot: slot % 2,
                address: base + 0x10_0000 + i * 0x1_0000,
                size: 0x4000 + (i as u32 % 4) * 0x2000,
                snapshot: None,
            });
            d.ubos.push(UboDetail {
                stage: Stage::Vertex,
                slot,
                address: base + 0x2000_0000 + (n % 300) * 0x1000 + u64::from(slot) * 0x100,
                size: 0x100,
                snapshot: None,
            });
        }
        for index in 0..2u32 {
            d.streams.push(StreamDetail {
                index,
                address: base + 0x3000_0000 + (n % 500) * 0x2000,
                size: 0x2000,
                stride: 16,
                divisor: 0,
            });
        }
        detail.draws.push(Arc::new(d));
    }
    // Slot 0 of each stage may be written.
    let rw_of = |_: &str, _: bool, slot: u32| slot == 0;
    renamer.note_rw_with(&detail, None, &rw_of);
    {
        let mut st = lock(&renamer.state);
        let n = st.cands.len();
        // Make 24 of the written candidates renamed: 8 each of Snapshot, Merge and Rename.
        let mut addrs: Vec<u64> = st
            .cands
            .iter()
            .filter(|(_, c)| !c.ro)
            .map(|(a, _)| *a)
            .collect();
        addrs.sort_unstable();
        for (k, a) in addrs.iter().take(24).enumerate() {
            let c = st.cands.get_mut(a).unwrap();
            c.learn.set(match k % 3 {
                0 => Class::Rename,
                1 => Class::Merge,
                _ => Class::Snapshot,
            });
            c.hits = 2;
            c.cmask = vec![1];
            c.gmask = vec![2];
        }
        eprintln!("renamer cost: {n} candidates");
    }
    let frames = 200;
    let t = std::time::Instant::now();
    // The encoder learns whether the programs write, then the flush places the bindings and sends it the few that it could
    // not place.
    renamer.note_rw_with(&detail, None, &rw_of);
    let check = {
        let mut st = lock(&renamer.state);
        let (bindings, _) = st.bindings(&detail);
        st.adopt(&bindings)
    };
    eprintln!("  bindings left for the encoder: {}", check.len());
    for _ in 0..frames {
        renamer.note_rw_with(&detail, Some(&check), &rw_of);
    }
    let note = t.elapsed();
    let t = std::time::Instant::now();
    for k in 0..frames {
        let c = renamer.at_flush(&detail, k + 1);
        std::hint::black_box(c);
    }
    let flush = t.elapsed();
    let time = |name: &str, f: &dyn Fn()| {
        let t = std::time::Instant::now();
        for _ in 0..frames {
            f();
        }
        eprintln!(
            "  {name}: {:.3} ms",
            t.elapsed().as_secs_f64() * 1e3 / frames as f64
        );
    };
    time("bindings", &|| {
        std::hint::black_box(lock(&renamer.state).bindings(&detail));
    });
    let (bs, _) = lock(&renamer.state).bindings(&detail);
    eprintln!("  distinct bindings: {}", bs.len());
    time("adopt", &|| {
        lock(&renamer.state).adopt(&bs);
    });
    time("others", &|| {
        std::hint::black_box(others(&detail));
    });
    let o = others(&detail);
    let ranges: Vec<(u64, u32)> = (0..24)
        .map(|i| (base + 0x10_0000 + i * 0x1_0000, 0x4000))
        .collect();
    time("blocked_flags", &|| {
        std::hint::black_box(blocked_flags(&ranges, o.iter().copied()));
    });
    eprintln!(
        "renamer cost per frame: note_rw {:.3} ms, at_flush {:.3} ms",
        note.as_secs_f64() * 1e3 / frames as f64,
        flush.as_secs_f64() * 1e3 / frames as f64
    );
}

fn key(prog: u64, slot: u32, size: u32) -> Key {
    Key {
        prog,
        slot_stage: slot,
        size: size.next_multiple_of(256),
    }
}

fn binding(addr: u64, size: u32, k: Key) -> Binding {
    Binding {
        addr,
        size,
        key: k,
        at: (0, 0, 0),
    }
}

/// A state that has learned from the range at `SUM` that the binding `k` is a merge range.
fn taught(k: Key) -> State {
    let mut st = State::default();
    st.candidate(SUM, 0x40);
    {
        let c = st.cands.get_mut(&SUM).unwrap();
        c.learn.set(Class::Merge);
        c.hits = SNAPSHOT_HITS;
        c.cmask = vec![0b0010];
        c.gmask = vec![0b0100];
    }
    st.adopt(&[binding(SUM, 0x40, k)]);
    st.push_keys(SUM);
    st
}

#[test]
fn a_new_address_of_a_known_key_gets_the_class_and_masks_at_once() {
    let k = key(7, 1, 0x40);
    let mut st = taught(k);
    assert_eq!(st.keys[&k].class, Class::Merge);
    // The ring comes round at another address, with a size of the same block.
    st.adopt(&[binding(SUM + 0x1000, 0x30, k)]);
    let c = &st.cands[&(SUM + 0x1000)];
    assert_eq!(c.learn.class(), Class::Merge);
    assert!(c.derived);
    assert_eq!(
        (c.cmask.clone(), c.gmask.clone()),
        (vec![0b0010], vec![0b0100])
    );
    // The next flush captures it as a merge range, with no learning frame.
    let g = Guest(RefCell::new(vec![0; 0x2000]));
    let f = st.flush(
        1,
        &[(SUM + 0x1000, 0x30)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert_eq!(f.renamed.len(), 1);
    assert_eq!(f.renamed[0].kind, Kind::Merge);
    assert!(!f.sampling);
    // A binding of another program or slot is not taught.
    st.adopt(&[binding(SUM + 0x2000, 0x40, key(8, 1, 0x40))]);
    assert!(!st.cands.contains_key(&(SUM + 0x2000)));
}

#[test]
fn a_range_that_a_snapshot_binding_also_reads_stays_a_merge_range() {
    let (km, ks) = (key(1, 1, 0x40), key(2, 2, 0x40));
    let mut st = taught(km);
    st.keys.insert(
        ks,
        KeyInfo {
            class: Class::Snapshot,
            ro: true,
            cmask: Vec::new(),
            gmask: Vec::new(),
            grown: 0,
        },
    );
    st.adopt(&[
        binding(SUM + 0x1000, 0x40, km),
        binding(SUM + 0x1000, 0x40, ks),
    ]);
    let c = &st.cands[&(SUM + 0x1000)];
    assert_eq!(c.learn.class(), Class::Merge);
    assert_eq!(c.keys, vec![km, ks]);
}

#[test]
fn bindings_that_need_a_merge_and_a_renamed_range_cannot_be_combined() {
    let (km, kr) = (key(1, 1, 0x40), key(2, 2, 0x40));
    let mut st = taught(km);
    st.keys.insert(
        kr,
        KeyInfo {
            class: Class::Rename,
            ro: false,
            cmask: Vec::new(),
            gmask: Vec::new(),
            grown: 0,
        },
    );
    let before = nxkit_nvn::diag::total(Reason::OverlapKeyDisagree);
    st.adopt(&[
        binding(SUM + 0x1000, 0x40, km),
        binding(SUM + 0x1000, 0x40, kr),
    ]);
    assert_eq!(st.cands[&(SUM + 0x1000)].learn.class(), Class::Conflict);
    assert!(nxkit_nvn::diag::total(Reason::OverlapKeyDisagree) > before);
    // The frame waits in full.
    let g = Guest(RefCell::new(vec![0; 0x2000]));
    let f = st.flush(
        1,
        &[(SUM + 0x1000, 0x40)],
        &|r| vec![false; r.len()],
        &|| Vec::new(),
        &|a, b| g.read(a, b),
    );
    assert!(f.conflict && f.renamed.is_empty());
}

#[test]
fn a_range_that_is_never_for_its_own_reasons_does_not_unteach_the_key() {
    let k = key(1, 1, 0x40);
    let mut st = taught(k);
    st.adopt(&[binding(SUM + 0x1000, 0x40, k)]);
    // The other address is found in a pool of raw stores and demoted.
    assert!(st.demote(SUM + 0x1000));
    st.push_keys(SUM + 0x1000);
    assert_eq!(st.keys[&k].class, Class::Merge);
    // A binding that a shader writes does unteach a snapshot key.
    let ks = key(5, 1, 0x40);
    st.keys.insert(
        ks,
        KeyInfo {
            class: Class::Snapshot,
            ro: true,
            cmask: Vec::new(),
            gmask: Vec::new(),
            grown: 0,
        },
    );
    st.adopt(&[binding(SUM + 0x3000, 0x40, ks)]);
    st.candidate(SUM + 0x3000, 0x40);
    assert_eq!(st.keys[&ks].class, Class::Never);
}

#[test]
fn the_distinct_bindings_of_a_frame_are_found_once_and_keyed_by_block() {
    let mut detail = FrameDetail::default();
    let prog: Arc<str> = Arc::from("prog");
    for size in [100u32, 100, 200] {
        let mut d = crate::walk::empty_draw();
        d.program.vertex = prog.clone();
        d.sbufs.push(crate::detail::UboDetail {
            stage: Stage::Vertex,
            slot: 1,
            address: SUM,
            size,
            snapshot: None,
        });
        detail.draws.push(Arc::new(d));
    }
    let mut st = State::default();
    let (b, ranges) = st.bindings(&detail);
    assert_eq!(ranges, vec![(SUM, 100), (SUM, 200)]);
    // Two sizes, one key.
    assert_eq!(b.len(), 2);
    assert_eq!(b[0].key, b[1].key);
    assert_eq!(b[0].key.size, 256);
}

#[test]
fn a_range_that_passed_the_check_against_other_uses_is_trusted_for_a_while() {
    let g = Guest(RefCell::new(vec![0; 0x400]));
    let mut st = State::default();
    st.watch_read_only(SUM, 0x40);
    st.cands.get_mut(&SUM).unwrap().learn.set(Class::Snapshot);
    let flush = |st: &mut State, others: &BlockedBy| {
        let fence = st.frame + 1;
        st.flush(fence, &[(SUM, 0x40)], others, &|| Vec::new(), &|a, b| {
            g.read(a, b)
        })
    };
    // The check is made until the range passed it CLEAR_RUN times; a use that overlaps resets the count.
    for _ in 0..CLEAR_RUN {
        assert_eq!(flush(&mut st, &|r| vec![false; r.len()]).renamed.len(), 1);
    }
    assert_eq!(st.cands[&SUM].clear_run, CLEAR_RUN);
    let f = flush(&mut st, &|_| panic!("the check is skipped"));
    assert_eq!(f.renamed.len(), 1);
    // Once in 60 flushes it is made again, and a use that overlaps now stops the copy.
    while !(st.frame + 1).is_multiple_of(60) {
        flush(&mut st, &|_| panic!("the check is skipped"));
    }
    let f = flush(&mut st, &|r| vec![true; r.len()]);
    assert!(f.renamed.is_empty());
    assert_eq!(st.cands[&SUM].clear_run, 0);
}
