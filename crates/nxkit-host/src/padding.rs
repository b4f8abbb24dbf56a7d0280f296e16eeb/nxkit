//! Inspect unused image padding without taking ownership of dyld's reservation.
//! dyld reserves the entire image before mapping segments. Padding can therefore
//! already be allocated, or a loader may leave a hole. Neither belongs to us to
//! remap. The GPU buffer starts after the image and does not need this padding.
use crate::{HostError, Result};
const PAGE: u64 = 0x4000;

pub(crate) fn gaps(
    segments: impl IntoIterator<Item = (u64, u64)>,
    slide: i64,
    from: u64,
    to: u64,
) -> Result<Vec<(u64, u64)>> {
    if from > to {
        return Err(HostError::Memory("Invalid image padding range".into()));
    }
    let mut spans = Vec::new();
    for (base, len) in segments {
        let start = base
            .checked_add_signed(slide)
            .ok_or_else(|| HostError::Memory("Slid segment address overflow".into()))?;
        let end = start
            .checked_add(len)
            .ok_or_else(|| HostError::Memory("Segment end overflow".into()))?;
        spans.push((start, end));
    }
    spans.sort_unstable();
    let mut cursor = from;
    let mut result = Vec::new();
    for (start, end) in spans {
        if cursor >= to {
            break;
        }
        if start > cursor {
            result.push((cursor, start.min(to) - cursor));
        }
        cursor = cursor.max(end);
    }
    if cursor < to {
        result.push((cursor, to - cursor));
    }
    Ok(result)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Padding {
    pub mapped: u64,
    pub unreported: u64,
}

/// `region` only queries an address; no allocation/protection operation is supplied.
/// Existing mappings retain their original protections, including inaccessible ones.
/// A missing VM report (a hole or a failed query) causes no memory operation.
/// Genuine holes remain holes. Guest data, code and key pages occupy verified
/// Mach-O segments, not this padding; no legitimate loader write targets a gap.
pub(crate) fn inspect(
    gaps: &[(u64, u64)],
    mut region: impl FnMut(u64) -> Option<(u64, u64)>,
) -> Result<Padding> {
    let mut result = Padding::default();
    for &(start, len) in gaps {
        let end = start
            .checked_add(len)
            .ok_or_else(|| HostError::Memory("Image padding end overflow".into()))?;
        let mut cursor = start;
        while cursor < end {
            let (next, mapped) = match region(cursor) {
                Some((base, size)) => {
                    let limit = base
                        .checked_add(size)
                        .ok_or_else(|| HostError::Memory("VM region end overflow".into()))?;
                    if base > cursor || limit <= cursor {
                        return Err(HostError::Memory(
                            "VM region does not cover image padding query".into(),
                        ));
                    }
                    (end.min(limit), true)
                }
                None => (end.min(cursor.saturating_add(PAGE)), false),
            };
            let total = if mapped {
                &mut result.mapped
            } else {
                &mut result.unreported
            };
            *total = total
                .checked_add(next - cursor)
                .ok_or_else(|| HostError::Memory("Image padding size overflow".into()))?;
            cursor = next;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn already_reserved_padding_is_retained_without_an_allocator() {
        let mut queries = 0;
        let p = inspect(&[(0x1c000, PAGE)], |at| {
            queries += 1;
            assert_eq!(at, 0x1c000);
            Some((0x1c000, PAGE))
        })
        .unwrap();
        assert_eq!(
            p,
            Padding {
                mapped: PAGE,
                unreported: 0
            }
        );
        assert_eq!(queries, 1);
    }
    #[test]
    fn a_genuine_hole_is_left_unmapped() {
        let mut queries = 0;
        let p = inspect(&[(0x10000, 2 * PAGE)], |_| {
            queries += 1;
            None
        })
        .unwrap();
        assert_eq!(
            p,
            Padding {
                mapped: 0,
                unreported: 2 * PAGE
            }
        );
        assert_eq!(queries, 2);
    }
    #[test]
    fn mixed_and_partial_regions_are_bounded_to_padding() {
        let p = inspect(&[(0x10000, 3 * PAGE)], |at| match at {
            0x10000 => Some((0x8000, 3 * PAGE)),
            0x14000 => None,
            0x18000 => Some((0x18000, 10 * PAGE)),
            _ => panic!("unexpected query"),
        })
        .unwrap();
        assert_eq!(
            p,
            Padding {
                mapped: 2 * PAGE,
                unreported: PAGE
            }
        );
        assert!(inspect(&[(0x10000, PAGE)], |_| Some((0x20000, PAGE))).is_err());
        assert!(inspect(&[(0x10000, PAGE)], |_| Some((0x8000, PAGE))).is_err());
    }
    #[test]
    fn gaps_clip_outside_and_overlapping_segments() {
        assert_eq!(
            gaps([(0, PAGE), (8 * PAGE, PAGE)], 0, 2 * PAGE, 6 * PAGE).unwrap(),
            [(2 * PAGE, 4 * PAGE)]
        );
        assert_eq!(
            gaps(
                [(PAGE, 2 * PAGE), (2 * PAGE, 2 * PAGE), (5 * PAGE, 2 * PAGE)],
                0,
                2 * PAGE,
                6 * PAGE
            )
            .unwrap(),
            [(4 * PAGE, PAGE)]
        );
        assert!(gaps([], 0, PAGE, PAGE).unwrap().is_empty());
        assert!(gaps([(0, 4 * PAGE)], 0, PAGE, 3 * PAGE).unwrap().is_empty());
    }
    #[test]
    fn gaps_follow_actual_slide_and_exclude_segments_and_tail() {
        let base = 0x100_0000_0000;
        let slide = 0x1_0000_0000i64 - base as i64;
        let p = gaps(
            [
                (base - PAGE, PAGE),
                (base, 3 * PAGE),
                (base + 4 * PAGE, PAGE),
            ],
            slide,
            0x1_0000_0000,
            0x1_0000_0000 + 5 * PAGE,
        )
        .unwrap();
        assert_eq!(p, [(0x1_0000_c000, PAGE)]);
        assert!(gaps([(0, PAGE)], -1, 0, PAGE).is_err());
        assert!(gaps([(u64::MAX, PAGE)], 0, 0, PAGE).is_err());
        assert!(gaps([], 0, 2, 1).is_err());
    }
}
