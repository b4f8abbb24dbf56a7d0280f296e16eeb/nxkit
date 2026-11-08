//! Tests of the CPU reference.

use super::*;
use crate::half::{from_f32, to_f32};
use crate::net::BLOB_LEN;

fn bytes(h: &[u16]) -> Vec<u8> {
    h.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A small deterministic generator for weights.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
    fn halves(&mut self, n: usize, scale: f32) -> Vec<u8> {
        bytes(
            &(0..n)
                .map(|_| from_f32(self.next() * scale))
                .collect::<Vec<_>>(),
        )
    }
}

fn tensor(w: usize, h: usize, c: usize, mut f: impl FnMut(usize, usize, usize) -> f32) -> Tensor {
    let mut t = Tensor::zeros(w, h, c);
    for y in 0..h {
        for x in 0..w {
            for k in 0..c {
                t.pixel_mut(x, y)[k] = from_f32(f(x, y, k));
            }
        }
    }
    t
}

#[test]
fn hmma_sums_in_the_documented_order() {
    // 2^24 + 1 rounds back to 2^24 in f32, so the 1 is lost and the sum is 0. A different order would give 1.
    let mut a = [0u16; 16];
    let mut b = [0u16; 16];
    a[0] = from_f32(4096.0);
    b[0] = from_f32(4096.0);
    a[1] = from_f32(1.0);
    b[1] = from_f32(1.0);
    a[2] = from_f32(-4096.0);
    b[2] = from_f32(4096.0);
    assert_eq!(hmma_step(&a, &b, ZERO), ZERO);
    // C is added last: (2^24 - 2^24) + 3 would be the other order of the first two; here the 1 is added after.
    assert_eq!(to_f32(hmma_step(&a, &b, from_f32(3.0))), 3.0);
}

#[test]
fn hmma_rounds_to_half_after_the_step() {
    let mut a = [0u16; 16];
    let mut b = [0u16; 16];
    a[0] = from_f32(1.0);
    b[0] = from_f32(2048.0);
    a[1] = from_f32(1.0);
    b[1] = from_f32(1.0);
    // 2049 is a tie in half and goes to even.
    assert_eq!(to_f32(hmma_step(&a, &b, ZERO)), 2048.0);
}

#[test]
fn conv3x3_on_a_tiny_tensor_by_hand() {
    // 2x2 pixels, 16 channels, input value 1.0 in channel 0 of every pixel and 0 elsewhere.
    let input = tensor(2, 2, 16, |_, _, k| if k == 0 { 1.0 } else { 0.0 });
    // Weights: every tap, cin 0 -> every cout = 0.5; everything else 0.
    let mut w = vec![ZERO; 9 * 16 * 16];
    for t in 0..9 {
        for n in 0..16 {
            w[(t * 16) * 16 + n] = from_f32(0.5);
        }
    }
    let mut bias = vec![ZERO; 16];
    bias[0] = from_f32(-10.0);
    bias[1] = from_f32(0.25);
    let out = conv3x3(&input, &bytes(&w), &bytes(&bias), true).unwrap();
    // Every output pixel of a 2x2 picture sees all 4 pixels in its 3x3 window, so D = 4 * 0.5 = 2.
    for y in 0..2 {
        for x in 0..2 {
            let px = out.pixel(x, y);
            assert_eq!(to_f32(px[0]), 0.0, "relu(2 - 10)");
            assert_eq!(to_f32(px[1]), 2.25);
            assert_eq!(to_f32(px[2]), 2.0);
        }
    }
    // A 1x1 picture sees only itself.
    let one = tensor(1, 1, 16, |_, _, k| if k == 0 { 1.0 } else { 0.0 });
    let out = conv3x3(&one, &bytes(&w), &bytes(&bias), false).unwrap();
    assert_eq!(to_f32(out.pixel(0, 0)[0]), -9.5);
}

#[test]
fn conv3x3_taps_use_the_right_offsets() {
    // Tap t reads pixel (x + t % 3 - 1, y + t / 3 - 1). Put a one-hot weight on tap 5 (dx = +1, dy = 0).
    let input = tensor(
        3,
        1,
        16,
        |x, _, k| if k == 0 { (x + 1) as f32 } else { 0.0 },
    );
    let mut w = vec![ZERO; 9 * 16 * 16];
    w[5 * 16 * 16] = from_f32(1.0);
    let out = conv3x3(&input, &bytes(&w), &bytes(&[ZERO; 16]), false).unwrap();
    assert_eq!(to_f32(out.pixel(0, 0)[0]), 2.0);
    assert_eq!(to_f32(out.pixel(1, 0)[0]), 3.0);
    assert_eq!(to_f32(out.pixel(2, 0)[0]), 0.0);
}

#[test]
fn down_reads_the_2x2_block() {
    let input = tensor(
        2,
        2,
        16,
        |x, y, k| if k == 0 { (1 + x + 2 * y) as f32 } else { 0.0 },
    );
    // Tap t has weight (t + 1) for cin 0 -> cout 0; cout 1 uses tap 3 only.
    let mut w = vec![ZERO; 4 * 16 * 16];
    for t in 0..4 {
        w[t * 256] = from_f32((t + 1) as f32);
    }
    w[3 * 256 + 1] = from_f32(1.0);
    let out = down2x2(&input, &bytes(&w), &bytes(&[ZERO; 16])).unwrap();
    assert_eq!((out.width, out.height), (1, 1));
    assert_eq!(to_f32(out.pixel(0, 0)[0]), 1.0 + 4.0 + 9.0 + 16.0);
    assert_eq!(to_f32(out.pixel(0, 0)[1]), 4.0);
    assert_eq!(
        down2x2(
            &tensor(3, 2, 16, |_, _, _| 0.0),
            &bytes(&w),
            &bytes(&[ZERO; 16])
        ),
        Err(Error::Shape("a down layer needs an even size"))
    );
}

#[test]
fn up_starts_from_the_skip() {
    let input = tensor(1, 1, 16, |_, _, k| if k == 0 { 2.0 } else { 0.0 });
    let skip = tensor(2, 2, 16, |x, y, k| {
        if k == 0 {
            (10 * (y * 2 + x)) as f32
        } else {
            0.0
        }
    });
    // Tap t, cin 0 -> cout 0 has weight t + 1.
    let mut w = vec![ZERO; 4 * 16 * 16];
    for t in 0..4 {
        w[t * 256] = from_f32((t + 1) as f32);
    }
    let out = up2x2(&input, &skip, &bytes(&w), &bytes(&[ZERO; 16])).unwrap();
    assert_eq!(to_f32(out.pixel(0, 0)[0]), 2.0);
    assert_eq!(to_f32(out.pixel(1, 0)[0]), 14.0);
    assert_eq!(to_f32(out.pixel(0, 1)[0]), 26.0);
    assert_eq!(to_f32(out.pixel(1, 1)[0]), 38.0);
    let bad = Tensor::zeros(1, 1, 16);
    assert!(up2x2(&input, &bad, &bytes(&w), &bytes(&[ZERO; 16])).is_err());
}

#[test]
fn space_to_depth_orders_the_channels() {
    let mut img = Image {
        width: 2,
        height: 2,
        data: vec![0; 16],
    };
    for (i, v) in img.data.iter_mut().enumerate() {
        *v = (i as u8) * 16;
    }
    let t = space_to_depth(&img).unwrap();
    assert_eq!((t.width, t.height, t.channels), (1, 1, 16));
    for k in 0..16 {
        assert_eq!(
            t.pixel(0, 0)[k],
            from_f32(f32::from(img.data[k]) / 255.0),
            "channel {k}"
        );
    }
}

#[test]
fn final_layer_adds_the_picture_and_unfolds_depth() {
    // One network pixel, zero weights, zero bias: the output is the picture itself.
    let input = Tensor::zeros(1, 1, 16);
    let picture = Image {
        width: 2,
        height: 2,
        data: vec![
            0, 51, 255, 255, 102, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255,
        ],
    };
    let out = final_layer(&input, &picture, &vec![0; 9 * 16 * 16 * 2], &[0; 32]).unwrap();
    // Pixel (0,0): R = 0, G = 51/255 = 0.2 -> 205 of 1023, B = 1.0.
    assert_eq!(out.data[0] & 0x3ff, 0);
    assert_eq!((out.data[0] >> 10) & 0x3ff, 205);
    assert_eq!((out.data[0] >> 20) & 0x3ff, 1023);
    assert_eq!(out.data[0] >> 30, 3);
    assert_eq!(out.data[1] & 0x3ff, 409);
    assert_eq!(out.data[3] >> 30, 3);
    // A bias on channel 2s + 1 for s = 1 (output pixel (1, 0), component G) shows up there only.
    let mut bias = vec![ZERO; 16];
    bias[3] = from_f32(0.5);
    let out = final_layer(&input, &picture, &vec![0; 9 * 16 * 16 * 2], &bytes(&bias)).unwrap();
    assert_eq!((out.data[1] >> 10) & 0x3ff, 512, "0 + 0.5, ties to even");
    assert_eq!(
        (out.data[0] >> 10) & 0x3ff,
        205,
        "other pixels are unchanged"
    );
}

#[test]
fn nv12_grey_stays_grey() {
    let luma = vec![128u8; 64];
    let chroma = vec![128u8; 16 * 2];
    let nv = Nv12 {
        width: 8,
        height: 8,
        luma: &luma,
        chroma: &chroma,
    };
    // Identity matrix, no offsets.
    let mut push = [0.0f32; 16];
    push[0] = 1.0;
    push[5] = 0.0;
    push[1] = 0.0;
    push[4] = 1.0;
    push[10] = 0.0;
    push[8] = 1.0;
    let img = convert_nv12(&nv, &push).unwrap();
    assert_eq!(&img.data[..4], &[128, 128, 128, 255]);
    assert!(
        convert_nv12(
            &Nv12 {
                width: 8,
                height: 8,
                luma: &luma[1..],
                chroma: &chroma
            },
            &push
        )
        .is_err()
    );
}

#[test]
fn chroma_is_filtered_bilinearly() {
    // Chroma 2 x 2: Cb ramps 0 to 255 along x. Output pixel x = 0 of a 4-wide luma sits at chroma x = -0.25, clamped.
    let luma = vec![0u8; 16 * 8];
    let mut chroma = vec![0u8; 8 * 4 * 2];
    for y in 0..4 {
        for x in 0..8 {
            chroma[(y * 8 + x) * 2] = (x * 36) as u8;
        }
    }
    let nv = Nv12 {
        width: 16,
        height: 8,
        luma: &luma,
        chroma: &chroma,
    };
    // r = U' * w1 only.
    let mut push = [0.0f32; 16];
    push[1] = 1.0;
    let img = convert_nv12(&nv, &push).unwrap();
    // Pixel x = 2: u = 2.5 / 16 * 8 = 1.25 texels -> between texels 0 and 1 at 0.75: (0.25 * 0 + 0.75 * 36).
    assert_eq!(img.data[2 * 4], unorm8(0.75 * 36.0 / 255.0));
    // Pixel x = 0 clamps to texel 0.
    assert_eq!(img.data[0], 0);
}

fn synthetic_weights() -> (Vec<u8>, Vec<u8>) {
    let mut g = Lcg(7);
    let mut blob = Vec::with_capacity(BLOB_LEN);
    for l in &LAYERS {
        blob.resize(l.weight_offset, 0);
        let fan_in = (l.kind.taps() * l.in_channels) as f32;
        blob.extend(g.halves(l.weight_bytes() / 2, 1.5 / fan_in.sqrt()));
        blob.resize(l.bias_offset, 0);
        blob.extend(g.halves(l.out_channels, 0.1));
    }
    let res = g.halves(RESAMPLER_LEN / 2, 0.3);
    (blob, res)
}

fn crop() -> (Vec<u8>, Vec<u8>) {
    let luma: Vec<u8> = (0..64 * 32).map(|i| ((i * 7) % 220 + 16) as u8).collect();
    let chroma: Vec<u8> = (0..32 * 16 * 2)
        .map(|i| ((i * 13) % 200 + 20) as u8)
        .collect();
    (luma, chroma)
}

fn pushes(w: usize, h: usize) -> ([f32; 16], [u32; 21]) {
    let conv = [
        1.164,
        0.0,
        1.793,
        0.0,
        1.164,
        -0.213,
        -0.533,
        0.0,
        1.164,
        2.112,
        0.0,
        0.0,
        -16.0 / 255.0,
        -0.5,
        -0.5,
        0.0,
    ];
    let mut r = [0u32; 21];
    r[2] = (w - 1) as u32;
    r[3] = (h - 1) as u32;
    r[10] = (1.0f32 / (w.div_ceil(6) * 8) as f32).to_bits();
    r[11] = (1.0f32 / (h.div_ceil(6) * 8) as f32).to_bits();
    (conv, r)
}

#[test]
fn full_chain_on_a_crop_is_deterministic() {
    let (blob, res) = synthetic_weights();
    let (luma, chroma) = crop();
    let nv = Nv12 {
        width: 64,
        height: 32,
        luma: &luma,
        chroma: &chroma,
    };
    let (conv, rp) = pushes(64, 32);
    let a = run_chain(&nv, &conv, &rp, None, &blob, &res).unwrap();
    let b = run_chain(&nv, &conv, &rp, None, &blob, &res).unwrap();
    assert_eq!(a, b);
    assert_eq!((a.output.width, a.output.height), (88, 48));
    assert_eq!(a.output.data.len(), 88 * 48 * 4);
    assert_eq!(a.layers.len(), 11);
    assert_eq!(
        (a.layers[4].width, a.layers[4].height, a.layers[4].channels),
        (8, 4, 64)
    );
    assert_eq!(
        (
            a.layers[10].width,
            a.layers[10].height,
            a.layers[10].channels
        ),
        (32, 16, 16)
    );
    assert_eq!((a.final_picture.width, a.final_picture.height), (64, 32));
    assert!(a.output.data.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    // The output is not flat: the network and the picture reach it.
    assert!(a.output.data.iter().any(|&v| v != a.output.data[0]));
    // The same with quantisation, a flip and a dither: still deterministic, and it changes something.
    let mut rq = rp;
    rq[18] = 47;
    rq[19] = 1;
    rq[20] = 1;
    let dither = Dither {
        width: 4,
        height: 4,
        values: (0..16).map(|i| i as f32 / 16.0).collect(),
    };
    let q = run(&nv, &conv, &rq, Some(&dither), &blob, &res).unwrap();
    assert_eq!(q, run(&nv, &conv, &rq, Some(&dither), &blob, &res).unwrap());
    assert_ne!(q, a.output);
}

#[test]
fn run_layer_matches_the_direct_call() {
    let (blob, _) = synthetic_weights();
    let mut g = Lcg(3);
    let input = tensor(8, 6, 16, |_, _, _| g.next());
    let l = &LAYERS[1];
    let direct = conv3x3(
        &input,
        l.weights(&blob).unwrap(),
        l.bias(&blob).unwrap(),
        true,
    )
    .unwrap();
    assert_eq!(run_layer(l, &input, None, &blob).unwrap(), direct);
    assert!(run_layer(&LAYERS[7], &Tensor::zeros(2, 2, 64), None, &blob).is_err());
    assert!(run_layer(&LAYERS[11], &input, None, &blob).is_err());
}

#[test]
fn resampler_with_zero_weights_is_the_bilinear_picture() {
    let w = 12;
    let h = 12;
    let data: Vec<u32> = (0..w * h)
        .map(|i| pack10([(i % 12) as f32 / 12.0, 0.5, 0.25, 1.0]))
        .collect();
    let pic = Picture10 {
        width: w,
        height: h,
        data,
    };
    let (_, push) = pushes(w, h);
    let out = resample(&pic, &vec![0; RESAMPLER_LEN], &push, None).unwrap();
    assert_eq!((out.width, out.height), (16, 16));
    // Green is constant 0.5 (512 of 1023, as a half) everywhere: 128 of 255.
    assert!(
        out.data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[1] == 128 && p[2] == 64 && p[3] == 255)
    );
    assert!(resample(&pic, &[0; 4], &push, None).is_err());
}
