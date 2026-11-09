//! Golden values from the Python model `refl.py` on the synthetic texture and blob of `reference/tests.rs` (no game
//! data): see `tests/fixtures/golden_light.txt`.

use super::*;
use crate::half;
use crate::reference::Picture;
use crate::reference::tests::{synth_blob, synth_picture};
use nxb_model::{sha256, to_hex};

const GOLDEN: &str = include_str!("../../tests/fixtures/golden_light.txt");

fn hex32(s: &str) -> u32 {
    u32::from_str_radix(s, 16).unwrap()
}

#[test]
fn matches_the_python_model() {
    let blob = synth_blob();
    let mut lines = GOLDEN.lines().filter(|l| !l.starts_with('#'));
    let mut cases = 0;
    while let Some(head) = lines.next() {
        let f: Vec<&str> = head.split_whitespace().collect();
        assert_eq!(f[0], "case", "{head}");
        let net = Net::by_key(f[1]).unwrap().1;
        let n = |i: usize| f[i].parse::<u32>().unwrap();
        let (w_in, h_in, w_out, h_out) = (n(4), n(5), n(6), n(7));
        let (nin, nout) = (u32::from(net.in_px), u32::from(net.out_px));
        let fl = |i: usize| f32::from_bits(hex32(f[i]));
        let push = LightPush {
            offset: (n(8) as i32, n(9) as i32),
            tile: (
                u32::from(net.step_x) * nout / nin,
                u32::from(net.step_y) * nout / nin,
            ),
            scale: [fl(10), fl(11)],
            origin: [fl(12), fl(13)],
            out: (w_out, h_out),
            rcp_out: [1.0 / w_out as f32, 1.0 / h_out as f32],
            strength: fl(14),
        };
        let tex = synth_picture(w_in as usize, h_in as usize);
        let layers = net.blob().layers();
        let weights = Weights::from_vr_blob(&blob[..layers * crate::net::LAYER_BYTES]).unwrap();
        let t = run_tile(net, &weights, &push, &tex, n(2) as usize, n(3) as usize).unwrap();
        let what = format!("{} tg ({}, {})", net.key, f[2], f[3]);
        let pix: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
        assert_eq!(t.pixels.len(), pix[1].parse::<usize>().unwrap(), "{what}");
        let grid: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
        let mut g = Vec::new();
        t.grid
            .cells
            .iter()
            .for_each(|v| g.extend(half::from_f32(*v).to_le_bytes()));
        assert_eq!(
            (t.grid.width, t.grid.height),
            (grid[1].parse().unwrap(), grid[2].parse().unwrap())
        );
        assert_eq!(to_hex(&sha256(&g).0), grid[3], "grid of {what}");
        for at in [0, t.pixels.len() / 2, t.pixels.len() - 1] {
            let px: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
            let p = t.pixels[at];
            let want: Vec<u32> = px[3..7].iter().map(|v| hex32(v)).collect();
            let got: Vec<u32> = p.rgba.iter().map(|c| c.to_bits()).collect();
            assert_eq!(
                ((p.x, p.y), got),
                ((px[1].parse().unwrap(), px[2].parse().unwrap()), want),
                "{what}"
            );
        }
        let mut b = Vec::new();
        for p in &t.pixels {
            b.extend(p.x.to_le_bytes());
            b.extend(p.y.to_le_bytes());
            p.rgba
                .iter()
                .for_each(|c| b.extend(c.to_bits().to_le_bytes()));
        }
        assert_eq!(to_hex(&sha256(&b).0), pix[2], "pixels of {what}");
        cases += 1;
    }
    assert_eq!(cases, 20);
}

#[test]
fn the_push_words_round_trip() {
    let p = LightPush {
        offset: (3, 1),
        tile: (64, 32),
        scale: [1.0, 0.5],
        origin: [0.25, 0.0],
        out: (2048, 1152),
        rcp_out: [1.0 / 2048.0, 1.0 / 1152.0],
        strength: 2.5,
    };
    let w = p.words([1536, 864]);
    assert_eq!(LightPush::from_words(&w), Some(p));
    assert_eq!((w[2], w[3], w[6], w[7]), (1535, 863, 2047, 1151));
    assert!(LightPush::from_words(&w[..28]).is_none());
}

#[test]
fn a_flat_picture_with_zero_weights_stays_flat() {
    // The network adds nothing, every pixel is the square of the colour and the sharpening of a flat luma is zero.
    let tex = Picture::from_fn(96, 48, |_, _| [0.5, 0.25, 0.75, 1.0]);
    let net = Net::by_key("fa3033d05c0a41d6").unwrap().1;
    let push = LightPush::from_words(&{
        let mut w = [0u32; 29];
        w[18] = 64;
        w[19] = 32;
        w[20] = 1.0f32.to_bits();
        w[21] = 1.0f32.to_bits();
        w[24] = 128;
        w[25] = 64;
        w[26] = (1.0f32 / 128.0).to_bits();
        w[27] = (1.0f32 / 64.0).to_bits();
        w[28] = 2.5f32.to_bits();
        w
    })
    .unwrap();
    let img = run_image(net, &[0; 0x1800], &push, &tex, [96, 48]).unwrap();
    assert!(img.rgba.iter().all(|p| *p == [0.25, 0.0625, 0.5625, 1.0]));
    // vr programs and blobs of the wrong length are refused.
    let vr = Net::by_key("14c10aa70b81f04e").unwrap().1;
    assert_eq!(
        run_image(vr, &[0; 0x1800], &push, &tex, [96, 48]).unwrap_err(),
        Error::NotVr
    );
    assert_eq!(
        run_image(net, &[0; 0x3000], &push, &tex, [96, 48]).unwrap_err(),
        Error::BadBlob
    );
}
