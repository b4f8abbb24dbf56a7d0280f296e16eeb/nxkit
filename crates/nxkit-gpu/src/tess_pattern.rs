//! Candidate uniform fractional-odd triangle pattern for a patch replay prototype.
//!
//! The three-edge segmentation and construction of the inner ring follow OpenGL 4.6 Core,
//! section 11.2.2.1. The specification permits implementations to choose the triangulation
//! between rings. The choice here needs comparison against the target GPU before production use.
//! This file is standalone so the pattern can be checked on systems without Metal.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PatternError {
    InvalidLevel,
    NonUniform,
    OverThree,
}

pub(crate) type Coordinate = [f32; 3];

pub(crate) const CAPTURE_WORDS: usize = 144;

const CORNERS: [Coordinate; 3] = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];

/// Coordinates in the order of a triangle-list draw, or an empty list for a culled patch.
/// Inner level 1 and outer level 3 are unused for a triangular domain.
pub(crate) fn pattern(
    inner: [f32; 2],
    outer: [f32; 4],
    counterclockwise: bool,
) -> Result<Vec<Coordinate>, PatternError> {
    let outer = &outer[..3];
    if outer.iter().any(|v| v.is_nan() || *v <= 0.) {
        return Ok(Vec::new());
    }
    let levels = [inner[0], outer[0], outer[1], outer[2]];
    if levels.iter().any(|x| !x.is_finite()) {
        return Err(PatternError::InvalidLevel);
    }
    let levels = levels.map(|l| l.max(1.));
    let l = levels[0];
    if levels.iter().any(|x| *x != l) {
        return Err(PatternError::NonUniform);
    }
    if l > 3. {
        return Err(PatternError::OverThree);
    }
    let mut vertices = Vec::with_capacity(if l == 1. { 3 } else { 39 });
    let mut push = |a, b, c| {
        if counterclockwise {
            vertices.extend([a, b, c]);
        } else {
            vertices.extend([a, c, b]);
        }
    };
    if l == 1. {
        push(CORNERS[0], CORNERS[1], CORNERS[2]);
        return Ok(vertices);
    }

    // The middle segment has length 1/l. Both short end segments have the same length.
    let short = (1. - l.recip()) * 0.5;
    // Each inner corner is the intersection of lines normal to the two adjacent outer edges,
    // through the nearest subdivision points. In an equilateral triangle the two non-corner
    // barycentric weights are equal and add to the projected short distance with factor 3/2.
    let inset = short * (2. / 3.);
    let ring = CORNERS.map(|corner| corner.map(|x| if x == 1. { 1. - 2. * inset } else { inset }));
    for i in 0..3 {
        let j = (i + 1) % 3;
        let (a, b) = (CORNERS[i], CORNERS[j]);
        let (p, q) = (lerp(a, b, short), lerp(b, a, short));
        let (c, d) = (ring[i], ring[j]);
        push(a, p, c);
        push(p, q, d);
        push(p, d, c);
        push(q, b, d);
    }
    push(ring[0], ring[1], ring[2]);
    Ok(vertices)
}

fn lerp(a: Coordinate, b: Coordinate, t: f32) -> Coordinate {
    std::array::from_fn(|i| a[i] * (1. - t) + b[i] * t)
}

pub(crate) fn capture_descriptor(address: u64, first: u32, count: u32) -> Vec<u8> {
    // MSL aligns uint4 to 16: the pointer is followed by 8 bytes of padding.
    let mut d = Vec::with_capacity(336);
    d.extend(address.to_le_bytes());
    d.extend([0; 8]);
    for v in [first, count, 0, 1, CAPTURE_WORDS as u32] {
        d.extend(v.to_le_bytes());
    }
    for i in 0..CAPTURE_WORDS as i16 {
        d.extend(i.to_le_bytes());
    }
    d.resize(d.len().next_multiple_of(16), 0);
    d
}

/// Builds the little-endian float4/uint4 records expected by the domain vertex function.
/// Each record stores the ordinal of the patch's first capture vertex, then the patch ID.
pub(crate) fn coordinate_records(
    pattern: &[Coordinate],
    patch_size: u32,
    patches: u32,
    bytes: usize,
) -> Option<Vec<u8>> {
    let expected = (patches as usize)
        .checked_mul(pattern.len())?
        .checked_mul(32)?;
    if patch_size == 0 || bytes != expected {
        return None;
    }
    let mut coords = Vec::with_capacity(bytes);
    for patch in 0..patches {
        let first = patch.checked_mul(patch_size)?;
        for [a, b, c] in pattern.iter().copied() {
            for v in [a.to_bits(), b.to_bits(), c.to_bits(), 0, first, patch, 0, 0] {
                coords.extend(v.to_le_bytes());
            }
        }
    }
    Some(coords)
}

/// Bytes of the indexed diagnostic shader descriptor, not including the separate GPU pointer.
/// Direct NVN draws name the first element by address, so their element offset is zero here.
pub(crate) fn index_descriptor(
    width: u32,
    count: u32,
    base_vertex: i32,
    max_vertex: u32,
) -> Option<[u8; 32]> {
    if !matches!(width, 1 | 2 | 4) {
        return None;
    }
    let bytes = u64::from(width) * u64::from(count);
    let mut data = [0; 32];
    data[..8].copy_from_slice(&bytes.to_le_bytes());
    for (part, word) in
        data[8..]
            .chunks_exact_mut(4)
            .zip([0, count, base_vertex as u32, width, max_vertex, 0])
    {
        part.copy_from_slice(&word.to_le_bytes());
    }
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn twice_area(t: &[Coordinate]) -> f32 {
        (t[1][1] - t[0][1]) * (t[2][2] - t[0][2]) - (t[1][2] - t[0][2]) * (t[2][1] - t[0][1])
    }

    #[test]
    fn unit_and_culling() {
        assert_eq!(
            pattern([1., 42.], [1., 1., 1., 42.], true).unwrap(),
            CORNERS
        );
        for cull in [-1., -0., 0., f32::NAN] {
            assert!(
                pattern([1.; 2], [1., cull, 1., 1.], true)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn nonuniform_or_large_is_not_silently_approximated() {
        assert_eq!(
            pattern([2.; 2], [2., 1., 2., 2.], true),
            Err(PatternError::NonUniform)
        );
        assert_eq!(
            pattern([4.; 2], [4.; 4], true),
            Err(PatternError::OverThree)
        );
        assert_eq!(
            pattern([f32::NAN; 2], [2.; 4], true),
            Err(PatternError::InvalidLevel)
        );
    }

    #[test]
    fn nonunit_patterns_tile_the_triangle_with_consistent_winding() {
        for factor in [1.0001, 2., 2.5, 3.] {
            for sign in [-1., 1.] {
                let v = pattern([factor; 2], [factor; 4], sign == 1.).unwrap();
                assert_eq!(v.len(), 39);
                let mut distinct = v.clone();
                distinct.sort_by_key(|a| a.map(f32::to_bits));
                distinct.dedup();
                assert_eq!(distinct.len(), 12);
                assert!(v.iter().all(
                    |p| p.iter().all(|c| *c >= 0.) && (p.iter().sum::<f32>() - 1.).abs() < 1e-6
                ));
                let area: f32 = v
                    .chunks_exact(3)
                    .map(|t| sign * twice_area(t))
                    .inspect(|a| assert!(*a > 0.))
                    .sum();
                assert!((area - 1.).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn generated_vertices_point_to_the_original_capture_patch() {
        let v = coordinate_records(&CORNERS, 4, 2, 32 * 6).unwrap();
        let word = |n: usize| u32::from_le_bytes(v[n * 4..n * 4 + 4].try_into().unwrap());
        assert_eq!(word(4), 0);
        assert_eq!(word(5), 0);
        assert_eq!(word(24), 1f32.to_bits());
        assert_eq!(word(28), 4);
        assert_eq!(word(29), 1);
        assert_eq!(coordinate_records(&CORNERS, 0, 2, 32 * 6), None);
        assert_eq!(coordinate_records(&CORNERS, 4, 2, 32 * 5), None);
    }

    #[test]
    fn capture_state_has_the_msl_layout() {
        let d = capture_descriptor(0x12_3456_7890, 2, 17);
        let u32_at = |off| u32::from_le_bytes(d[off..off + 4].try_into().unwrap());
        assert_eq!(d.len(), 336);
        assert_eq!(
            u64::from_le_bytes(d[0..8].try_into().unwrap()),
            0x12_3456_7890
        );
        assert_eq!(u32_at(16), 2);
        assert_eq!(u32_at(20), 17);
        assert_eq!(u32_at(28), 1);
        assert_eq!(u32_at(32), 144);
        assert_eq!(i16::from_le_bytes(d[322..324].try_into().unwrap()), 143);
    }

    #[test]
    fn indexed_descriptor_preserves_the_signed_base_and_exact_byte_range() {
        let d = index_descriptor(2, 7, -4, 91).unwrap();
        assert_eq!(u64::from_le_bytes(d[0..8].try_into().unwrap()), 14);
        assert_eq!(u32::from_le_bytes(d[8..12].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(d[12..16].try_into().unwrap()), 7);
        assert_eq!(i32::from_le_bytes(d[16..20].try_into().unwrap()), -4);
        assert_eq!(u32::from_le_bytes(d[20..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(d[24..28].try_into().unwrap()), 91);
        assert_eq!(index_descriptor(3, 7, 0, 0), None);
    }
}
