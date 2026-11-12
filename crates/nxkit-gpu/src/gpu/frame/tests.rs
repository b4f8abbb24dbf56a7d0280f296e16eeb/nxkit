//! Tests of the lead chunk that streams the first part of a frame.

use super::*;

#[test]
fn a_lead_chunk_takes_the_first_eighth_of_the_weight_and_the_chunks_keep_their_ends() {
    let weights = vec![10u32; 40];
    let mut ends = jobs::split(&weights, 4);
    assert_eq!(ends, vec![10, 20, 30, 40]);
    assert!(lead_chunk(&weights, &mut ends, 8));
    assert_eq!(ends, vec![5, 10, 20, 30, 40]);
}

#[test]
fn no_lead_chunk_for_a_short_frame_one_chunk_a_full_ring_or_a_big_first_pass() {
    let weights = vec![10u32; 40];
    let mut one = vec![40];
    assert!(!lead_chunk(&weights, &mut one, 8));
    assert_eq!(one, vec![40]);
    let mut ends = jobs::split(&weights, 4);
    assert!(
        !lead_chunk(&weights, &mut ends, 5),
        "1 + 5 command buffers do not fit 5"
    );
    assert_eq!(ends.len(), 4);
    let short = vec![10u32; 4];
    let mut e = jobs::split(&short, 2);
    assert!(!lead_chunk(&short, &mut e, 8));
    let heavy = [1000u32, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    let mut e = vec![1, 6, 10];
    assert!(
        !lead_chunk(&heavy, &mut e, 8),
        "the first chunk is the lead already"
    );
    assert_eq!(e, vec![1, 6, 10]);
}
