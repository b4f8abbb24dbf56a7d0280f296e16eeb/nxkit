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
    let b = params(&r([1, 2], [2048, 1152]), &r([3, 4], [1280, 720]), true, 0);
    let w: Vec<u32> = b
        .chunks(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert_eq!(w, [1, 2, 2048, 1152, 3, 4, 1280, 720, 1, 0, 0, 0]);
}

#[test]
fn equal_size_mirrored_copies_need_the_kernel() {
    let src = TextureRegion::whole(0, 0, [3, 2, 1]);
    for flags in [0, LINEAR_FILTER_BIT, 2, LINEAR_FILTER_BIT | 2] {
        assert!(!needs_kernel(&src, &src, flags));
        for mirror in [MIRROR_X_BIT, MIRROR_Y_BIT, MIRROR_X_BIT | MIRROR_Y_BIT] {
            assert!(needs_kernel(&src, &src, flags | mirror));
        }
    }
    let dst = TextureRegion::whole(0, 0, [6, 4, 1]);
    assert!(needs_kernel(&src, &dst, 0));
}

#[test]
fn in_place_overlap_uses_real_spans_not_texture_identity() {
    for (src, dst) in [
        ((100, 40), (100, 40)),
        ((100, 40), (120, 80)),
        ((120, 80), (100, 40)),
        ((100, 100), (120, 10)),
        ((120, 10), (100, 100)),
        ((u64::MAX - 3, 4), (u64::MAX, 1)),
        ((u64::MAX, 1), (u64::MAX - 3, 4)),
    ] {
        assert!(in_place_overlaps(Some(src), Some(dst)), "{src:?}, {dst:?}");
    }
}

#[test]
fn in_place_overlap_allows_private_disjoint_and_empty_storage() {
    let span = Some((100, 40));
    assert!(!in_place_overlaps(None, None));
    assert!(!in_place_overlaps(None, span));
    assert!(!in_place_overlaps(span, None));
    for (src, dst) in [
        ((100, 40), (140, 20)),
        ((140, 20), (100, 40)),
        ((100, 40), (200, 40)),
        ((100, 40), (120, 0)),
        ((120, 0), (100, 40)),
        ((u64::MAX - 3, 3), (u64::MAX, 1)),
        ((u64::MAX, 1), (u64::MAX - 3, 3)),
    ] {
        assert!(!in_place_overlaps(Some(src), Some(dst)), "{src:?}, {dst:?}");
    }
}

#[test]
fn the_parameters_keep_the_mirror_axes_separate_from_the_filter() {
    let src = TextureRegion::whole(2, 3, [3, 2, 1]);
    let dst = TextureRegion::whole(4, 5, [6, 4, 1]);
    for linear in [false, true] {
        for (flags, axes) in [(0, [0, 0]), (4, [1, 0]), (8, [0, 1]), (12, [1, 1])] {
            // ENGINE_2D and LINEAR_FILTER do not select a mirror axis.
            let b = params(&src, &dst, linear, flags | 3);
            assert_eq!(&b[32..36], &u32::from(linear).to_le_bytes());
            assert_eq!(&b[36..40], &u32::to_le_bytes(axes[0]));
            assert_eq!(&b[40..44], &u32::to_le_bytes(axes[1]));
            assert_eq!(&b[44..48], &[0; 4]);
        }
    }
}

/// Run the kernel's CPU reference with its actual parameter bytes on an asymmetric image.
fn copy_reference(src: TextureRegion, dst: TextureRegion, flags: u32, linear: bool) -> Vec<f32> {
    let b = params(&src, &dst, linear, flags);
    let w: Vec<u32> = b
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let source: Vec<f32> = (0..20).map(|v| v as f32).collect();
    let mut out = vec![-1.0; 10 * 7];
    let read = |x: u32, y: u32| source[((w[1] + y) * 5 + w[0] + x) as usize];
    let mix = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
    for y in 0..w[7] {
        for x in 0..w[6] {
            let dx = sample_at(x, w[6], w[9] != 0);
            let dy = sample_at(y, w[7], w[10] != 0);
            let v = if w[8] == 0 {
                read(nearest(dx, w[2], w[6]), nearest(dy, w[3], w[7]))
            } else {
                let (x0, x1, tx) = taps(dx, w[2], w[6]);
                let (y0, y1, ty) = taps(dy, w[3], w[7]);
                mix(
                    mix(read(x0, y0), read(x1, y0), tx),
                    mix(read(x0, y1), read(x1, y1), tx),
                    ty,
                )
            };
            out[((w[5] + y) * 10 + w[4] + x) as usize] = v;
        }
    }
    out
}

fn offset_region(size: [u32; 3], origin: [u32; 3]) -> TextureRegion {
    TextureRegion {
        origin,
        ..TextureRegion::whole(0, 0, size)
    }
}

fn assert_region(out: &[f32], region: TextureRegion, expected: &[f32]) {
    for y in 0..7 {
        for x in 0..10 {
            let inside = (region.origin[0]..region.origin[0] + region.size[0]).contains(&x)
                && (region.origin[1]..region.origin[1] + region.size[1]).contains(&y);
            let want = if inside {
                expected[((y - region.origin[1]) * region.size[0] + x - region.origin[0]) as usize]
            } else {
                -1.0
            };
            assert!(
                (out[(y * 10 + x) as usize] - want).abs() < 1e-6,
                "at {x},{y}"
            );
        }
    }
}

#[test]
fn mirrors_reverse_only_the_requested_axes_in_offset_regions() {
    let src = offset_region([3, 2, 1], [1, 1, 0]);
    let dst = offset_region([3, 2, 1], [2, 3, 0]);
    for (flags, want) in [
        (0, [6.0, 7.0, 8.0, 11.0, 12.0, 13.0]),
        (4, [8.0, 7.0, 6.0, 13.0, 12.0, 11.0]),
        (8, [11.0, 12.0, 13.0, 6.0, 7.0, 8.0]),
        (12, [13.0, 12.0, 11.0, 8.0, 7.0, 6.0]),
    ] {
        for linear in [false, true] {
            assert_region(&copy_reference(src, dst, flags, linear), dst, &want);
        }
    }
}

#[test]
fn mirrored_scaling_preserves_nearest_and_linear_mapping() {
    let src = offset_region([3, 2, 1], [1, 1, 0]);
    let dst = offset_region([2, 4, 1], [2, 1, 0]);
    assert_region(
        &copy_reference(src, dst, 12, false),
        dst,
        &[13.0, 11.0, 13.0, 11.0, 8.0, 6.0, 8.0, 6.0],
    );
    assert_region(
        &copy_reference(src, dst, 12, true),
        dst,
        &[12.75, 11.25, 11.5, 10.0, 9.0, 7.5, 7.75, 6.25],
    );
}

#[test]
fn mirrored_nearest_maps_the_centre_before_selecting_a_texel() {
    // At an exact source boundary, reflecting an already selected texel would choose the wrong neighbour.
    let at: Vec<u32> = (0..2)
        .map(|d| nearest(sample_at(d, 2, true), 4, 2))
        .collect();
    assert_eq!(at, [3, 1]);
    assert_eq!(sample_at(0, 1, true), 0);
}
