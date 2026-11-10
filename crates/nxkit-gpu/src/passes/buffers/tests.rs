use super::*;

#[test]
fn an_aligned_copy_moves_words_four_at_a_time() {
    let j = Job::copy(0x1000, 0x9000, 0x100).unwrap();
    assert_eq!(j.mode, WORDS);
    assert_eq!((j.per, j.threads, j.serial), (4, 16, false));
}

#[test]
fn an_unaligned_copy_or_size_moves_bytes() {
    for (src, dst, n) in [
        (0x1001, 0x9000, 0x100),
        (0x1000, 0x9002, 0x100),
        (0x1000, 0x9000, 7),
    ] {
        let j = Job::copy(src, dst, n).unwrap();
        assert_eq!(j.mode, 0, "{src:#x} {dst:#x} {n}");
        assert_eq!(u64::from(j.threads), n.div_ceil(16));
    }
}

#[test]
fn nothing_to_copy_gives_no_job() {
    assert_eq!(Job::copy(0x1000, 0x2000, 0), None);
    assert_eq!(Job::copy(0x1000, 0x1000, 64), None);
    assert_eq!(Job::fill(0x1000, 0, 7), None);
}

#[test]
fn overlapping_ranges_run_on_one_thread_in_the_memmove_direction() {
    let up = Job::copy(0x1000, 0x1010, 0x100).unwrap();
    assert_eq!(
        (up.threads, up.serial, up.mode),
        (1, true, WORDS | BACKWARD)
    );
    assert_eq!(up.per, 0x40);
    let down = Job::copy(0x1010, 0x1000, 0x100).unwrap();
    assert_eq!((down.threads, down.serial, down.mode), (1, true, WORDS));
    // Ranges that touch do not overlap.
    let apart = Job::copy(0x1000, 0x1100, 0x100).unwrap();
    assert!(!apart.serial && apart.threads > 1);
}

#[test]
fn a_fill_keeps_its_pattern_and_picks_the_unit_by_alignment() {
    let j = Job::fill(0x2000, 0x40, 0xdead_beef).unwrap();
    assert_eq!((j.mode, j.value, j.threads), (FILL | WORDS, 0xdead_beef, 4));
    let j = Job::fill(0x2001, 0x40, 1).unwrap();
    assert_eq!(j.mode, FILL);
    let j = Job::fill(0x2000, 6, 1).unwrap();
    assert_eq!((j.mode, j.threads), (FILL, 1));
}

#[test]
fn a_huge_range_stays_inside_the_thread_limit() {
    let bytes = 8u64 << 30;
    let j = Job::copy(0x1_0000_0000, 0x5_0000_0000, bytes).unwrap();
    assert!(u64::from(j.threads) <= MAX_THREADS);
    assert!(u64::from(j.threads) * j.per >= bytes / 4);
}

#[test]
fn the_job_bytes_follow_the_layout_of_the_kernel() {
    let j = Job::fill(0x2000, 0x40, 0xa1b2_c3d4).unwrap();
    let b = j.to_bytes();
    let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    assert_eq!((u64_at(0), u64_at(8), u64_at(16)), (0, 0x2000, 0x40));
    assert_eq!(
        (u32_at(24), u32_at(28), u64_at(32)),
        (0xa1b2_c3d4, FILL | WORDS, 4)
    );
}
