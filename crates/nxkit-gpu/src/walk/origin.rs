//! The NVN window origin and the supported clip-space viewport swizzle.
//!
//! Window coordinates name the same target rows in NVN and Metal. A lower-left origin changes the direction within
//! the viewport, not its rectangle or the scissor. The device origin controls that direction. The origin captured
//! when the queue was created controls the front-face convention independently.

/// Positive X, Y, Z and W in `NVNviewportSwizzle`.
pub(crate) const IDENTITY: [u32; 4] = [0, 2, 4, 6];
const NEGATIVE_Y: [u32; 4] = [0, 3, 4, 6];

/// Only identity and negative Y can use the implemented viewport transform. Axis exchanges and negative X, Z or W
/// need another path. The caller reports them and keeps the origin conversion without the unsupported swizzle.
pub(crate) fn supported(swizzle: [u32; 4]) -> bool {
    matches!(swizzle, IDENTITY | NEGATIVE_Y)
}

pub(crate) fn viewport(mut v: [f32; 4], lower_left: bool, swizzle: [u32; 4]) -> [f32; 4] {
    if lower_left != (swizzle == NEGATIVE_Y) {
        v[1] += v[3];
        v[3] = -v[3];
    }
    v
}

/// Clip-space swizzling affects facing before the front-face test. It does not change the winding convention.
pub(crate) fn front_face(face: u32, queue_lower_left: bool) -> u32 {
    face ^ u32::from(queue_lower_left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_left_changes_direction_inside_an_asymmetric_subviewport() {
        let v = [11.0, 17.0, 59.0, 31.0];
        assert_eq!(viewport(v, false, IDENTITY), v);
        assert_eq!(viewport(v, true, IDENTITY), [11.0, 48.0, 59.0, -31.0]);
        // Metal maps NDC +1 to origin Y and NDC -1 to origin Y + height.
        let y = |v: [f32; 4], ndc: f32| v[1] + (1.0 - ndc) * v[3] * 0.5;
        let lower = viewport(v, true, IDENTITY);
        assert_eq!((y(lower, -1.0), y(lower, 1.0)), (17.0, 48.0));
        let upper = viewport(v, false, IDENTITY);
        assert_eq!((y(upper, -1.0), y(upper, 1.0)), (48.0, 17.0));
    }

    #[test]
    fn negative_y_composes_with_device_origin_without_moving_the_rectangle() {
        let v = [11.0, 17.0, 59.0, 31.0];
        assert_eq!(viewport(v, true, NEGATIVE_Y), v);
        assert_eq!(viewport(v, false, NEGATIVE_Y), [11.0, 48.0, 59.0, -31.0]);
        let scale = |v: [f32; 4]| v.map(|x| x * 1.5);
        assert_eq!(
            scale(viewport(v, true, IDENTITY)),
            [16.5, 72.0, 88.5, -46.5]
        );
    }

    #[test]
    fn queue_origin_alone_converts_both_front_face_conventions() {
        for face in [0, 1] {
            assert_eq!(front_face(face, false), face);
            assert_eq!(front_face(face, true), 1 - face);
        }
    }

    #[test]
    fn other_swizzles_are_explicitly_unsupported() {
        let v = [11.0, 17.0, 59.0, 31.0];
        assert!(supported(IDENTITY));
        assert!(supported(NEGATIVE_Y));
        for swizzle in [[1, 2, 4, 6], [2, 0, 4, 6], [0, 2, 5, 6], [0, 2, 4, 7]] {
            assert!(!supported(swizzle));
            assert_eq!(viewport(v, true, swizzle), viewport(v, true, IDENTITY));
        }
    }
}
