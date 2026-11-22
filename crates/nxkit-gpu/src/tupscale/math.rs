//! The arithmetic of the temporal upscale: the view values from the uniform buffer, the velocity of a pixel and the
//! conversion into the motion and the jitter that MetalFX takes. The motion kernel (`msl.rs`) does the per-pixel part
//! on the GPU. Its functions here (`ndc_of`, `decode_velocity`, `motion_pixels`) are the reference the tests compare
//! the kernel with.
//!
//! Clip space has `y` up. A texture row grows downwards when the pass draws with an upper-left origin (`row_sign` 1)
//! and upwards with a lower-left one (`row_sign` -1, a viewport with a negative height). MetalFX takes pixels with `x`
//! to the right and `y` to the next row.

/// A 4x4 matrix by rows, for a row vector (`v * M`).
pub(crate) type Rows = [[f32; 4]; 4];

/// The signs that the conversions multiply with. They are 1 unless a knob flips one (`tupjx`, `tupjy`, `tupmx`,
/// `tupmy`), so a wrong sign can be found while the game runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Signs {
    /// The factors of the motion in x and y.
    pub(crate) motion: [f32; 2],
    /// The factors of the jitter in x and y.
    pub(crate) jitter: [f32; 2],
}

impl Signs {
    /// No flip.
    #[cfg(test)]
    pub(crate) const PLAIN: Self = Self {
        motion: [1.0; 2],
        jitter: [1.0; 2],
    };

    /// The signs with the knobs that are on now.
    pub(crate) fn from_knobs() -> Self {
        let f = |k: &str| if crate::knob(k) { -1.0 } else { 1.0 };
        Self {
            motion: [f("tupmx"), f("tupmy")],
            jitter: [f("tupjx"), f("tupjy")],
        }
    }
}

/// `N` little-endian floats at byte `at` of `bytes`, or none when they do not fit.
pub(crate) fn floats<const N: usize>(bytes: &[u8], at: u32) -> Option<[f32; N]> {
    let at = at as usize;
    let part = bytes.get(at..at.checked_add(N * 4)?)?;
    let mut out = [0.0; N];
    for (o, c) in out.iter_mut().zip(part.as_chunks::<4>().0) {
        *o = f32::from_le_bytes(*c);
    }
    Some(out)
}

/// The matrix of 16 floats at byte `at`, by rows.
pub(crate) fn rows(bytes: &[u8], at: u32) -> Option<Rows> {
    let f: [f32; 16] = floats(bytes, at)?;
    Some(std::array::from_fn(|r| {
        std::array::from_fn(|c| f[r * 4 + c])
    }))
}

/// `v * m`.
pub(crate) fn mul_row(v: [f32; 4], m: &Rows) -> [f32; 4] {
    std::array::from_fn(|c| (0..4).map(|r| v[r] * m[r][c]).sum())
}

/// The clip position of the centre of pixel `p` of a picture of `size` pixels.
#[cfg(test)]
pub(crate) fn ndc_of(p: [u32; 2], size: [u32; 2], row_sign: f32) -> [f32; 2] {
    let x = (p[0] as f32 + 0.5) / size[0] as f32 * 2.0 - 1.0;
    let y = 1.0 - (p[1] as f32 + 0.5) / size[1] as f32 * 2.0;
    [x, y * row_sign]
}

/// The velocity of the camera at clip position `ndc` and depth `device_z`: this position minus the previous one. None
/// when the previous position has no `w`.
pub(crate) fn camera_velocity(ndc: [f32; 2], device_z: f32, m: &Rows) -> Option<[f32; 2]> {
    let prev = mul_row([ndc[0], ndc[1], device_z, 1.0], m);
    (prev[3].abs() > 1e-20).then(|| [ndc[0] - prev[0] / prev[3], ndc[1] - prev[1] / prev[3]])
}

/// The velocity of a texel `raw` of the velocity texture, with `y` up. None when the texel has no velocity and the
/// camera motion stands in.
#[cfg(test)]
pub(crate) fn decode_velocity(
    raw: [f32; 2],
    code: &crate::settings::VelocityCode,
) -> Option<[f32; 2]> {
    if code.zero_means_camera && raw == [0.0, 0.0] {
        return None;
    }
    let x = raw[0] * code.scale[0] + code.bias[0];
    let y = raw[1] * code.scale[1] + code.bias[1];
    Some([x, if code.y_up { y } else { -y }])
}

/// The motion that MetalFX takes for velocity `v`: from the pixel to its place in the previous frame, in input pixels.
#[cfg(test)]
pub(crate) fn motion_pixels(v: [f32; 2], size: [u32; 2], row_sign: f32, s: &Signs) -> [f32; 2] {
    [
        -v[0] * size[0] as f32 * 0.5 * s.motion[0],
        v[1] * size[1] as f32 * 0.5 * row_sign * s.motion[1],
    ]
}

/// The jitter that MetalFX takes for the jitter `j` of the projection in clip units, in input pixels.
pub(crate) fn jitter_pixels(j: [f32; 2], size: [u32; 2], row_sign: f32, s: &Signs) -> (f32, f32) {
    (
        j[0] * size[0] as f32 * 0.5 * s.jitter[0],
        -j[1] * size[1] as f32 * 0.5 * row_sign * s.jitter[1],
    )
}

/// The largest clip movement of a probe point between two frames that is not a cut: half the screen.
pub(crate) const CUT_NDC: f32 = 1.0;

/// The depth of the probe points of [`camera_cut`]: far from the camera, where a turn shows and a step forward does
/// not.
const CUT_DEPTH: f32 = 0.01;

/// True when the camera cut to another place: a probe point at the centre or near a corner of the screen, far from
/// the camera, moved more than [`CUT_NDC`], or the matrix has no finite answer.
pub(crate) fn camera_cut(m: &Rows, depth_reversed: bool) -> bool {
    let z = if depth_reversed {
        CUT_DEPTH
    } else {
        1.0 - CUT_DEPTH
    };
    [
        [0.0, 0.0],
        [-0.9, -0.9],
        [0.9, -0.9],
        [-0.9, 0.9],
        [0.9, 0.9],
    ]
    .iter()
    .any(|p| match camera_velocity(*p, z, m) {
        Some(v) => !(v[0].abs() <= CUT_NDC && v[1].abs() <= CUT_NDC),
        None => true,
    })
}

/// The largest difference of `m` from the identity, for the diagnostics.
pub(crate) fn off_identity(m: &Rows) -> f32 {
    let mut d = 0.0f32;
    for (r, row) in m.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            d = d.max((v - if r == c { 1.0 } else { 0.0 }).abs());
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::VelocityCode;

    const ID: Rows = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    /// The code of a velocity written as `raw = V * 0.2495 + 32767 / 65535`.
    fn code() -> VelocityCode {
        let half = 32767.0 / 65535.0;
        VelocityCode {
            scale: [1.0 / 0.2495; 2],
            bias: [-half / 0.2495; 2],
            y_up: true,
            zero_means_camera: true,
        }
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    #[test]
    fn a_velocity_texel_decodes_to_the_velocity_it_encodes() {
        let encode = |v: f32| v * 0.2495 + 32767.0 / 65535.0;
        let v = decode_velocity([encode(0.1), encode(-0.25)], &code()).unwrap();
        assert!(close(v, [0.1, -0.25]), "{v:?}");
        assert!(close(
            decode_velocity([encode(0.0); 2], &code()).unwrap(),
            [0.0; 2]
        ));
        let down = VelocityCode {
            y_up: false,
            ..code()
        };
        assert!(close(
            decode_velocity([encode(0.1), encode(-0.25)], &down).unwrap(),
            [0.1, 0.25]
        ));
    }

    #[test]
    fn a_zero_texel_means_camera_motion_only_when_the_code_says_so() {
        assert_eq!(decode_velocity([0.0, 0.0], &code()), None);
        let every = VelocityCode {
            zero_means_camera: false,
            ..code()
        };
        assert!(decode_velocity([0.0, 0.0], &every).is_some());
        assert!(decode_velocity([0.0, 0.5], &code()).is_some());
    }

    #[test]
    fn pixel_centres_map_to_clip_space_in_both_row_directions() {
        assert!(close(ndc_of([0, 0], [4, 2], 1.0), [-0.75, 0.5]));
        assert!(close(ndc_of([3, 1], [4, 2], 1.0), [0.75, -0.5]));
        assert!(close(ndc_of([0, 0], [4, 2], -1.0), [-0.75, -0.5]));
    }

    #[test]
    fn a_still_camera_has_no_velocity() {
        let v = camera_velocity([0.3, -0.6], 0.25, &ID).unwrap();
        assert!(close(v, [0.0, 0.0]));
        assert!(!camera_cut(&ID, true));
    }

    #[test]
    fn a_camera_pan_reprojects_by_the_translation_row() {
        // A row vector takes the translation from row 3: the previous clip position is this one plus (0.1, -0.2).
        let mut m = ID;
        m[3][0] = 0.1;
        m[3][1] = -0.2;
        let v = camera_velocity([0.3, -0.6], 0.25, &m).unwrap();
        assert!(close(v, [-0.1, 0.2]), "{v:?}");
        // The previous place is to the right and lower on the screen: +x pixels and +y rows.
        let mv = motion_pixels(v, [1920, 1080], 1.0, &Signs::PLAIN);
        assert!(close(mv, [96.0, 108.0]), "{mv:?}");
    }

    #[test]
    fn motion_points_to_the_previous_place_in_pixels() {
        // V is this position minus the previous one, with y up. +x clip is +x pixels, +y clip is -y rows.
        let s = Signs::PLAIN;
        assert!(close(
            motion_pixels([0.1, 0.0], [1920, 1080], 1.0, &s),
            [-96.0, 0.0]
        ));
        assert!(close(
            motion_pixels([0.0, 0.1], [1920, 1080], 1.0, &s),
            [0.0, 54.0]
        ));
        assert!(close(
            motion_pixels([0.0, 0.1], [1920, 1080], -1.0, &s),
            [0.0, -54.0]
        ));
        let flipped = Signs {
            motion: [-1.0, 1.0],
            ..s
        };
        assert!(close(
            motion_pixels([0.1, 0.0], [1920, 1080], 1.0, &flipped),
            [96.0, 0.0]
        ));
    }

    #[test]
    fn jitter_in_clip_units_becomes_pixels_with_y_down() {
        let s = Signs::PLAIN;
        let j = jitter_pixels([0.25 / 960.0, -0.125 / 540.0], [1920, 1080], 1.0, &s);
        assert!(close([j.0, j.1], [0.25, 0.125]), "{j:?}");
        let j = jitter_pixels([0.0, -0.125 / 540.0], [1920, 1080], -1.0, &s);
        assert!(close([j.0, j.1], [0.0, -0.125]), "{j:?}");
    }

    #[test]
    fn a_large_jump_of_the_camera_is_a_cut() {
        let mut m = ID;
        m[3][0] = 0.05;
        assert!(!camera_cut(&m, true), "a small pan");
        m[3][0] = 1.5;
        assert!(camera_cut(&m, true), "half a screen and more");
        let mut flat = ID;
        flat[3][3] = 0.0;
        assert!(camera_cut(&flat, true), "no w");
        let mut nan = ID;
        nan[0][0] = f32::NAN;
        assert!(camera_cut(&nan, true), "not finite");
    }

    #[test]
    fn view_values_are_read_from_their_byte_offsets() {
        let mut b = vec![0u8; 96];
        for (i, c) in b.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            c.copy_from_slice(&(i as f32).to_le_bytes());
        }
        let m = rows(&b, 16).unwrap();
        assert_eq!(m[0], [4.0, 5.0, 6.0, 7.0]);
        assert_eq!(m[3], [16.0, 17.0, 18.0, 19.0]);
        assert_eq!(floats::<2>(&b, 88), Some([22.0, 23.0]));
        assert_eq!(floats::<4>(&b, 88), None, "past the end");
        assert_eq!(off_identity(&ID), 0.0);
    }
}
