use super::*;

#[test]
fn nearest_takes_the_texel_under_the_centre() {
    // 2048 -> 1280: the factor is 1.6, texel 0 takes 0, texel 1 (centre 2.4) takes 2, the last takes the last.
    assert_eq!(nearest(0, 2048, 1280), 0);
    assert_eq!(nearest(1, 2048, 1280), 2);
    assert_eq!(nearest(2, 2048, 1280), 4);
    assert_eq!(nearest(1279, 2048, 1280), 2047);
    // 1152 -> 720 is the same factor 1.6.
    assert_eq!(nearest(719, 1152, 720), 1151);
}

#[test]
fn nearest_stays_inside_the_source() {
    for (src, dst) in [(1u32, 7u32), (3, 100), (100, 3), (5, 5), (1280, 2048)] {
        for d in 0..dst {
            assert!(nearest(d, src, dst) < src, "{d} {src} {dst}");
        }
    }
    assert_eq!(nearest(0, 0, 4), 0);
    assert_eq!(nearest(0, 4, 0), 0);
}

#[test]
fn nearest_of_an_equal_size_is_the_identity() {
    for d in 0..64 {
        assert_eq!(nearest(d, 64, 64), d);
    }
}

#[test]
fn nearest_of_an_enlargement_repeats_texels() {
    let at: Vec<u32> = (0..6).map(|d| nearest(d, 3, 6)).collect();
    assert_eq!(at, [0, 0, 1, 1, 2, 2]);
}

#[test]
fn taps_halve_between_two_texels() {
    // 4 -> 2: destination texel 0 has its centre at 1.0 source texels, between texels 0 and 1 at weight 0.5.
    assert_eq!(taps(0, 4, 2), (0, 1, 0.5));
    assert_eq!(taps(1, 4, 2), (2, 3, 0.5));
}

#[test]
fn taps_clamp_to_the_region() {
    // 2 -> 4: the first centre is at -0.25 and the last at 1.25, so the edge texel repeats.
    let (a, b, t) = taps(0, 2, 4);
    assert_eq!((a, b), (0, 0));
    assert!((t - 0.75).abs() < 1e-6);
    let (a, b, _) = taps(3, 2, 4);
    assert_eq!((a, b), (1, 1));
}

#[test]
fn taps_of_an_equal_size_hit_one_texel() {
    for d in 0..16 {
        let (a, _, t) = taps(d, 16, 16);
        assert_eq!((a, t), (d, 0.0));
    }
}

#[test]
fn the_parameters_are_the_regions_and_the_filter() {
    let r = |o: [u32; 2], s: [u32; 2]| TextureRegion {
        level: 0,
        slice: 0,
        origin: [o[0], o[1], 0],
        size: [s[0], s[1], 1],
    };
    let b = params(&r([1, 2], [2048, 1152]), &r([3, 4], [1280, 720]), true);
    let w: Vec<u32> = b
        .chunks(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert_eq!(w, [1, 2, 2048, 1152, 3, 4, 1280, 720, 1, 0, 0, 0]);
}
