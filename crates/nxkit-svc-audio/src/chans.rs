//! The channels of a frame between the outputs and the device: stereo, or 5.1 for spatial audio
//! ([`crate::engine::width`]). 5.1 has the order of the SDK: front left, front right, center, LFE, back left,
//! back right, which is also the order of `kAudioChannelLayoutTag_MPEG_5_1_A`.

/// The SDK's default downmix coefficient of the center and the back channels.
#[allow(
    clippy::approx_constant,
    reason = "0.7071 is the default downmix coefficient of the SDK, not 1/sqrt(2) to full precision"
)]
const SIDE: f32 = 0.7071;
/// The SDK's default downmix coefficient of the LFE channel.
const LFE: f32 = 0.2512;

/// The stereo of one 5.1 frame `f`, with the default downmix coefficients.
fn down(f: &[f32]) -> (f32, f32) {
    let c = f[2] * SIDE + f[3] * LFE;
    (f[0] + c + f[4] * SIDE, f[1] + c + f[5] * SIDE)
}

/// Reads `ch` interleaved PCM16 channels per frame from `bytes` into `dst`, which has `w` channels per frame (2 or 6),
/// times `gain`. Six channels on a stereo device are mixed down; fewer channels on a 5.1 device go to the front.
pub(crate) fn convert(bytes: &[u8], ch: usize, w: usize, gain: f32, dst: &mut [f32]) {
    let frame = 2 * ch.max(1);
    let mut six = [0.0f32; 6];
    for (f, o) in bytes.chunks_exact(frame).zip(dst.chunks_exact_mut(w)) {
        let rd = |j: usize| f32::from(i16::from_le_bytes([f[2 * j], f[2 * j + 1]])) * gain;
        o.fill(0.0);
        match ch {
            1 => (o[0], o[1]) = (rd(0), rd(0)),
            6 if w == 6 => o.iter_mut().enumerate().for_each(|(j, v)| *v = rd(j)),
            6 => {
                six.iter_mut().enumerate().for_each(|(j, v)| *v = rd(j));
                (o[0], o[1]) = down(&six);
            }
            _ => (o[0], o[1]) = (rd(0), rd(1)),
        }
    }
}

/// Mixes interleaved 5.1 `src` down to interleaved stereo `dst`, for a device that plays no spatial audio.
pub(crate) fn downmix(src: &[f32], dst: &mut [f32]) {
    for (f, o) in src
        .as_chunks::<6>()
        .0
        .iter()
        .zip(dst.as_chunks_mut::<2>().0)
    {
        (o[0], o[1]) = down(f);
    }
}

/// Converts the interleaved PCM16 5.1 or stereo frames of the renderer: `src` has `k` channels per frame (1, 2 or 6),
/// `dst` gets `w` (2 or 6). Q15 fixed point, as the SDK mixes down.
pub(crate) fn frames_i32(src: &[i64; 6], k: usize, w: usize, dst: &mut [i32]) {
    const CF: i64 = 32768;
    const CC: i64 = 23167;
    const CL: i64 = 8224;
    const CB: i64 = 23167;
    dst.fill(0);
    if k >= 6 && w == 6 {
        for (d, s) in dst.iter_mut().zip(src) {
            *d = *s as i32;
        }
    } else if k >= 6 {
        let c = src[2] * CC + src[3] * CL;
        dst[0] = ((src[0] * CF + c + src[4] * CB) >> 15) as i32;
        dst[1] = ((src[1] * CF + c + src[5] * CB) >> 15) as i32;
    } else {
        dst[0] = src[0] as i32;
        dst[1] = if k > 1 { src[1] } else { src[0] } as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn six_channels_stay_six_on_a_spatial_device() {
        let mut d = [9.0f32; 6];
        convert(&pcm(&[1, 2, 3, 4, 5, 6]), 6, 6, 1.0, &mut d);
        assert_eq!(d, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn stereo_goes_to_the_front_and_mono_to_both() {
        let mut d = [9.0f32; 12];
        convert(&pcm(&[100, -100]), 2, 6, 0.5, &mut d[..6]);
        convert(&pcm(&[8]), 1, 6, 1.0, &mut d[6..]);
        assert_eq!(d[..6], [50.0, -50.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(d[6..], [8.0, 8.0, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn the_downmix_is_the_sdks() {
        let mut a = [0.0f32; 2];
        convert(&pcm(&[1000, 0, 1000, 1000, 0, 1000]), 6, 2, 1.0, &mut a);
        let mut b = [0.0f32; 2];
        downmix(&[1000.0, 0.0, 1000.0, 1000.0, 0.0, 1000.0], &mut b);
        assert_eq!(a, b);
        assert!((a[0] - (1000.0 + 707.1 + 251.2)).abs() < 0.01);
        assert!((a[1] - (707.1 + 251.2 + 707.1)).abs() < 0.01);
    }

    #[test]
    fn the_renderer_frames_follow_the_width() {
        let s = [32768, 0, 32768, 0, 0, 32768];
        let mut six = [0i32; 6];
        frames_i32(&s, 6, 6, &mut six);
        assert_eq!(six, [32768, 0, 32768, 0, 0, 32768]);
        let mut st = [0i32; 2];
        frames_i32(&s, 6, 2, &mut st);
        assert_eq!(st, [32768 + 23167, 23167 + 23167]);
        let mut mono = [7i32; 6];
        frames_i32(&[5, 0, 0, 0, 0, 0], 1, 6, &mut mono);
        assert_eq!(mono, [5, 5, 0, 0, 0, 0]);
    }
}
