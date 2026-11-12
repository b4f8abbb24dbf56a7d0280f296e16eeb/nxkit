//! Tests of judging a frame, the learning of one range, the plan of a frame and the write-back of words.

use super::*;

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
