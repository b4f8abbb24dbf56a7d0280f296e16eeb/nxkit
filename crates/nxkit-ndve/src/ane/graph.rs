//! The network as a MIL graph: the U-Net, then (for [`Variant::Full`]) the resampler without its bilinear stage.

use super::mil::{Builder, Conv, Tensor};
use super::resampler::{self, halves};
use crate::half;
use crate::net::{BLOB_LEN, Kind, LAYERS, PICTURE_HEIGHT as H, PICTURE_WIDTH as W};

/// Which part of the network the model holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// The U-Net and the resampler's network without the bilinear stage: outputs `delta` and `final`.
    Full,
    /// The U-Net only: output `final`.
    UnetOnly,
}

/// `[tap][cin][cout]` to `[cout][cin][k][k]` (`tap = k dy + dx`); input channel `q` of the result is channel
/// `perm[q]` of the source when `perm` is given.
fn conv_w(w: &[u16], k: usize, cin: usize, cout: usize, perm: Option<&[usize]>) -> Vec<u16> {
    let mut out = vec![0u16; w.len()];
    for co in 0..cout {
        for q in 0..cin {
            let ci = perm.map_or(q, |p| p[q]);
            for t in 0..k * k {
                out[(co * cin + q) * k * k + t] = w[(t * cin + ci) * cout + co];
            }
        }
    }
    out
}

/// `[tap][cin][cout]` to `[cin][cout][2][2]`.
fn deconv_w(w: &[u16], cin: usize, cout: usize) -> Vec<u16> {
    let mut out = vec![0u16; w.len()];
    for ci in 0..cin {
        for co in 0..cout {
            for t in 0..4 {
                out[(ci * cout + co) * 4 + t] = w[(t * cin + ci) * cout + co];
            }
        }
    }
    out
}

/// The program's input: `x`, `[1, 4, 1080, 1920]`.
pub(super) fn input() -> Tensor {
    Tensor {
        name: "x".to_owned(),
        shape: vec![1, 4, H, W],
    }
}

/// Build the graph of `variant` from the weight blob and the resampler weights. Returns the output tensors in order.
pub(super) fn build(
    b: &mut Builder,
    blob: &[u8],
    res: &[u8],
    variant: Variant,
) -> Result<Vec<Tensor>, String> {
    if blob.len() != BLOB_LEN {
        return Err(format!("{} weight bytes, expected {BLOB_LEN}", blob.len()));
    }
    let x = input();
    let fin = unet(b, blob, &x)?;
    let mut outs = Vec::new();
    if variant == Variant::Full {
        let rb = resampler::blocks(res)?;
        let delta = resample(b, &rb, &fin);
        outs.push(b.identity(&delta, "delta"));
    }
    outs.push(b.identity(&fin, "final"));
    Ok(outs)
}

/// The U-Net: returns the clipped picture, which is the `final` output and the resampler's input.
fn unet(b: &mut Builder, blob: &[u8], x: &Tensor) -> Result<Tensor, String> {
    // pixel_unshuffle channel q = c * 4 + s; the network's channel k = 4 * s + c.
    let q_to_k: Vec<usize> = (0..16).map(|q| 4 * (q % 4) + q / 4).collect();
    let mut t = b.pixel_unshuffle(x, 2);
    let mut outs: Vec<Option<Tensor>> = vec![None; 13];
    for l in &LAYERS[..11] {
        let w = l
            .weights(blob)
            .map(halves)
            .ok_or_else(|| format!("layer {}: weights out of the blob", l.number))?;
        let bias = l
            .bias(blob)
            .map(halves)
            .ok_or_else(|| format!("layer {}: bias out of the blob", l.number))?;
        let (cin, cout) = (l.in_channels, l.out_channels);
        t = match l.kind {
            Kind::Conv3x3 => {
                let perm = (l.number == 1).then_some(q_to_k.as_slice());
                b.conv(
                    &t,
                    &Conv {
                        weight: &conv_w(&w, 3, cin, cout, perm),
                        wshape: [cout, cin, 3, 3],
                        bias: Some(&bias),
                        strides: [1, 1],
                        pad: Some([1, 1, 1, 1]),
                        groups: 1,
                        immediate: false,
                    },
                )
            }
            Kind::Down2x2 => b.conv(
                &t,
                &Conv {
                    weight: &conv_w(&w, 2, cin, cout, None),
                    wshape: [cout, cin, 2, 2],
                    bias: Some(&bias),
                    strides: [2, 2],
                    pad: None,
                    groups: 1,
                    immediate: false,
                },
            ),
            Kind::Up2x2 => {
                let up = b.conv_transpose2(&t, &deconv_w(&w, cin, cout), cout);
                let skip = l
                    .skip
                    .and_then(|s| outs.get(s).cloned().flatten())
                    .ok_or_else(|| format!("layer {}: no skip tensor", l.number))?;
                let s = b.add(&up, &skip);
                b.add_bias(&s, &bias)
            }
            Kind::Final => return Err("the final layer is not in the loop".to_owned()),
        };
        t = b.relu(&t);
        outs[l.number] = Some(t.clone());
    }
    // Layer 12: output channel q = c * 4 + s (c = 0..3) from the network channel n(c, s).
    let l = &LAYERS[11];
    let w = l
        .weights(blob)
        .map(halves)
        .ok_or("layer 12: weights out of the blob")?;
    let bias = l
        .bias(blob)
        .map(halves)
        .ok_or("layer 12: bias out of the blob")?;
    let cw = conv_w(&w, 3, 16, 16, None);
    let sel: Vec<usize> = (0..3)
        .flat_map(|c| (0..4).map(move |s| [2 * s, 2 * s + 1, 8 + 2 * s, 9 + 2 * s][c]))
        .collect();
    let mut sw = Vec::with_capacity(12 * 16 * 9);
    for &o in &sel {
        sw.extend_from_slice(&cw[o * 16 * 9..(o + 1) * 16 * 9]);
    }
    let sb: Vec<u16> = sel.iter().map(|&o| bias[o]).collect();
    let t = b.conv(
        &t,
        &Conv {
            weight: &sw,
            wshape: [12, 16, 3, 3],
            bias: Some(&sb),
            strides: [1, 1],
            pad: Some([1, 1, 1, 1]),
            groups: 1,
            immediate: false,
        },
    );
    let d = b.pixel_shuffle(&t, 2);
    let rgb = b.slice_by_size(x, [0; 4], [1, 3, H, W]);
    let p = b.add(&rgb, &d);
    let p = b.clip(&p, half::ZERO, half::ONE);
    Ok(p)
}

/// The resampler's network on the picture `p` (`[1, 3, 1080, 1920]`): the delta, `[1, 1, 1440, 2560]`.
fn resample(b: &mut Builder, blocks: &[resampler::Block], p: &Tensor) -> Tensor {
    let luma_w = [0.2126f64, 0.7152, 0.0722].map(half::from_f64);
    let luma = b.conv(
        p,
        &Conv {
            weight: &luma_w,
            wshape: [1, 3, 1, 1],
            bias: None,
            strides: [1, 1],
            pad: None,
            groups: 1,
            immediate: true,
        },
    );
    let lp = b.pad_replicate(&luma, [5, 7, 5, 7]);
    let u = b.pixel_unshuffle(&lp, 6);
    let h = b.conv(
        &u,
        &Conv {
            weight: &resampler::block0_stage1(&blocks[0]),
            wshape: [192, 36, 3, 1],
            bias: None,
            strides: [1, 1],
            pad: None,
            groups: 1,
            immediate: false,
        },
    );
    let y = b.conv(
        &h,
        &Conv {
            weight: &resampler::block0_stage2(&blocks[0]),
            wshape: [256, 12, 1, 3],
            bias: None,
            strides: [1, 1],
            pad: None,
            groups: 16,
            immediate: false,
        },
    );
    let mut x = b.relu(&y);
    for (bi, blk) in blocks.iter().enumerate().skip(1) {
        let last = bi == 5;
        let (w, cout) = resampler::dense(blk, last);
        let y = b.conv(
            &x,
            &Conv {
                weight: &w,
                wshape: [cout, 256, 1, 1],
                bias: None,
                strides: [1, 1],
                pad: None,
                groups: 1,
                immediate: false,
            },
        );
        x = if last { y } else { b.relu(&y) };
    }
    b.pixel_shuffle(&x, 8)
}
