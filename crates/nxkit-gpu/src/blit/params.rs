//! Source coordinates for the final display blit. The origin belongs to the queued image.

/// Two Metal `float4` values: origin and signed extent, followed by the inclusive texel bounds of the crop.
/// A zero crop size selects the whole source. Nonzero crops keep the existing clamp to the source extent.
pub(super) fn source(size: [u32; 2], crop: [i32; 4], lower_left: bool) -> [f32; 8] {
    let [width, height] = size.map(|v| v as f32);
    let [x, y, w, h] = crop.map(|v| v as f32);
    let [x, y, w, h] = if w > 0.0 && h > 0.0 {
        [x, y, w.min(width - x), h.min(height - y)]
    } else {
        [0.0, 0.0, width, height]
    };
    let (origin_y, extent_y) = if lower_left { (y + h, -h) } else { (y, h) };
    [x, origin_y, w, extent_y, x, y, x + w - 1.0, y + h - 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    // The arithmetic of the shared plain/sRGB fragment fetch, sampled at destination pixel centers.
    fn texel(params: [f32; 8], pixel: [u32; 2], target: [u32; 2]) -> [u32; 2] {
        std::array::from_fn(|axis| {
            let uv = (pixel[axis] as f32 + 0.5) / target[axis] as f32;
            (params[axis] + uv * params[axis + 2]).clamp(params[axis + 4], params[axis + 6]) as u32
        })
    }

    #[test]
    fn upper_left_keeps_asymmetric_rows_and_columns() {
        let p = source([9, 7], [0; 4], false);
        assert_eq!(p, [0.0, 0.0, 9.0, 7.0, 0.0, 0.0, 8.0, 6.0]);
        for y in 0..7 {
            for x in 0..9 {
                assert_eq!(texel(p, [x, y], [9, 7]), [x, y]);
            }
        }
    }

    #[test]
    fn lower_left_reverses_only_rows_of_the_full_image() {
        let p = source([9, 7], [0; 4], true);
        for y in 0..7 {
            for x in 0..9 {
                assert_eq!(texel(p, [x, y], [9, 7]), [x, 6 - y]);
            }
        }
    }

    #[test]
    fn both_origins_stay_inside_an_offset_nonsquare_crop() {
        for lower in [false, true] {
            let p = source([9, 7], [2, 1, 5, 3], lower);
            assert_eq!(&p[4..], &[2.0, 1.0, 6.0, 3.0]);
            for y in 0..3 {
                for x in 0..5 {
                    assert_eq!(
                        texel(p, [x, y], [5, 3]),
                        [x + 2, if lower { 3 - y } else { y + 1 }]
                    );
                }
            }
        }
    }

    #[test]
    fn downscaled_lower_left_uv_is_reversed_before_texel_selection() {
        let p = source([5, 4], [0; 4], true);
        assert_eq!(texel(p, [0, 0], [5, 2]), [0, 3]);
        assert_eq!(texel(p, [4, 1], [5, 2]), [4, 1]);
    }

    #[test]
    fn upscaled_crop_clamps_to_its_own_edge_texels() {
        let p = source([9, 7], [2, 1, 5, 3], true);
        assert_eq!(texel(p, [0, 0], [15, 9]), [2, 3]);
        assert_eq!(texel(p, [14, 8], [15, 9]), [6, 1]);
        assert_eq!(
            source([9, 7], [7, 5, 8, 8], true),
            [7.0, 7.0, 2.0, -2.0, 7.0, 5.0, 8.0, 6.0]
        );
    }
}
