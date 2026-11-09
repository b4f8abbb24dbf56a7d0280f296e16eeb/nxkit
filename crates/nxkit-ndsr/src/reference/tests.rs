//! Golden values from the Python model `refvr.py` on a synthetic texture and blob (no game data): see
//! `tests/fixtures/golden.txt`. The texture is `((3x + 5y + 61c + (hash(x, y, c) & 63)) mod 256) / 256` clamped to the
//! picture; the blob is an LCG of halves in -0.5 .. 0.5.

use super::*;
use crate::half;
use nxb_model::{sha256, to_hex};

const GOLDEN: &str = include_str!("../../tests/fixtures/golden.txt");

fn hsh(x: u32, y: u32, c: u32) -> u32 {
    let h = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263))
        .wrapping_add(c.wrapping_mul(2_147_483_647));
    let h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^ (h >> 16)
}

pub(super) fn synth_picture(w: usize, h: usize) -> Picture {
    Picture::from_fn(w, h, |x, y| {
        let c = |c: u32| {
            let v =
                (x as u32 * 3 + y as u32 * 5 + c * 61 + (hsh(x as u32, y as u32, c) & 63)) % 256;
            v as f32 / 256.0
        };
        [c(0), c(1), c(2), 1.0]
    })
}

pub(super) fn synth_blob() -> Vec<u8> {
    let mut s: u32 = 12345;
    (0..0x1800)
        .flat_map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = (((s >> 16) & 0x3ff) as i32 - 512) as f64 / 1024.0;
            half::from_f64(v).to_le_bytes()
        })
        .collect()
}

fn digest(f: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut b = Vec::new();
    f(&mut b);
    to_hex(&sha256(&b).0)
}

fn grid_digest(g: &Grid) -> String {
    digest(|b| {
        g.cells
            .iter()
            .for_each(|v| b.extend(half::from_f32(*v).to_le_bytes()))
    })
}

#[test]
fn matches_the_python_model() {
    let blob = synth_blob();
    let weights = Weights::from_vr_blob(&blob).unwrap();
    let mut lines = GOLDEN.lines().filter(|l| !l.starts_with('#')).peekable();
    let mut cases = 0;
    while let Some(head) = lines.next() {
        let f: Vec<&str> = head.split_whitespace().collect();
        assert_eq!(f[0], "case", "{head}");
        let (net, w) = (Net::by_key(f[1]).unwrap().1, |i: usize| {
            f[i].parse::<i64>().unwrap()
        });
        let push = Push {
            w_in: w(4) as u32,
            h_in: w(5) as u32,
            w_out: w(6) as u32,
            h_out: w(7) as u32,
            offset: (w(8) as i32, w(9) as i32),
            unsharp: f32::from_bits(u32::from_str_radix(f[10], 16).unwrap()),
            dither: f[11] == "1",
        };
        let tex = synth_picture(push.w_in as usize, push.h_in as usize);
        let t = run_tile(net, &weights, &push, &tex, w(2) as usize, w(3) as usize).unwrap();
        let what = format!("{} tg ({}, {})", net.key, f[2], f[3]);

        let pix: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
        assert_eq!(t.pixels.len(), pix[1].parse::<usize>().unwrap(), "{what}");
        let d = digest(|b| {
            for p in &t.pixels {
                b.extend(p.x.to_le_bytes());
                b.extend(p.y.to_le_bytes());
                p.rgba
                    .iter()
                    .for_each(|c| b.extend(c.to_bits().to_le_bytes()));
            }
        });
        let grid: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
        assert_eq!(
            (t.grid.width, t.grid.height),
            (grid[1].parse().unwrap(), grid[2].parse().unwrap()),
            "{what}"
        );
        assert_eq!(grid_digest(&t.grid), grid[3], "grid of {what}");
        let sharp: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
        assert_eq!(grid_digest(&t.sharp), sharp[1], "sharp grid of {what}");
        // The sample pixels: first, middle, last. They localise a mismatch of the digest.
        for at in [0, t.pixels.len() / 2, t.pixels.len() - 1] {
            let px: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
            let p = t.pixels[at];
            let want: Vec<u32> = px[3..7]
                .iter()
                .map(|v| u32::from_str_radix(v, 16).unwrap())
                .collect();
            let got: Vec<u32> = p.rgba.iter().map(|c| c.to_bits()).collect();
            assert_eq!(
                ((p.x, p.y), got),
                ((px[1].parse().unwrap(), px[2].parse().unwrap()), want),
                "{what}"
            );
        }
        assert_eq!(d, pix[2], "pixels of {what}");
        cases += 1;
    }
    assert_eq!(cases, 15);
}

fn push(w_in: u32, h_in: u32, w_out: u32, h_out: u32) -> Push {
    Push {
        w_in,
        h_in,
        w_out,
        h_out,
        offset: (0, 0),
        unsharp: 0.5,
        dither: true,
    }
}

#[test]
fn a_64x48_crop_is_deterministic_and_complete() {
    let blob = synth_blob();
    let tex = synth_picture(64, 48);
    for key in ["14c10aa70b81f04e", "e30678db92cf66ff"] {
        let net = Net::by_key(key).unwrap().1;
        let p = push(64, 48, 96, 72);
        let a = run_image(net, &blob, &p, &tex).unwrap();
        let b = run_image(net, &blob, &p, &tex).unwrap();
        assert_eq!((a.width, a.height), (96, 72));
        assert_eq!(a.rgba.len(), 96 * 72);
        // Every pixel is written exactly once (an unwritten one stays NaN) and is finite.
        assert!(
            a.rgba.iter().all(|p| p.iter().all(|c| c.is_finite())),
            "{key}"
        );
        let bits = |i: &Image| {
            i.rgba
                .iter()
                .flatten()
                .map(|c| c.to_bits())
                .collect::<Vec<_>>()
        };
        assert_eq!(bits(&a), bits(&b), "{key}");
        assert!(a.rgba.iter().all(|p| p[3] == 1.0));
    }
}

#[test]
fn the_first_and_last_tile_cover_the_picture_edges() {
    let blob = synth_blob();
    let weights = Weights::from_vr_blob(&blob).unwrap();
    let net = Net::by_key("14c10aa70b81f04e").unwrap().1;
    let p = push(80, 60, 96, 72);
    let tex = synth_picture(80, 60);
    let (gx, gy) = groups(net, &p);
    assert_eq!((gx, gy), (3, 3));
    let first = run_tile(net, &weights, &p, &tex, 0, 0).unwrap();
    let last = run_tile(net, &weights, &p, &tex, gx - 1, gy - 1).unwrap();
    assert_eq!((first.pixels[0].x, first.pixels[0].y), (0, 0));
    let l = last.pixels.last().unwrap();
    assert_eq!((l.x, l.y), (95, 71));
    assert_eq!((first.grid.width, first.grid.height), (48, 36));
}

#[test]
fn other_packs_are_refused() {
    let blob = synth_blob();
    let weights = Weights::from_vr_blob(&blob).unwrap();
    let tex = synth_picture(8, 8);
    let net = Net::by_key("9327ea56aafa15b9").unwrap().1;
    let err = run_tile(net, &weights, &push(8, 8, 8, 8), &tex, 0, 0).unwrap_err();
    assert_eq!(err, Error::NotVr);
    let net = Net::by_key("14c10aa70b81f04e").unwrap().1;
    assert_eq!(
        run_image(net, &blob[..0x1800], &push(8, 8, 8, 8), &tex).unwrap_err(),
        Error::BadBlob
    );
}

#[test]
fn a_flat_picture_with_zero_weights_resamples_to_itself() {
    // Zero weights: the network adds nothing, the grid is the square of the base sample, and a flat texture stays flat
    // through the unsharp mask and the resample. Variant B has no encode: the pixel is the linear colour.
    let weights = Weights::from_vr_blob(&vec![0; network::BLOB_LEN]).unwrap();
    let tex = Picture::from_fn(40, 30, |_, _| [0.5, 0.5, 0.5, 1.0]);
    let net = Net::by_key("f407ab4429b8eae6").unwrap().1;
    let mut p = push(40, 30, 48, 36);
    p.dither = false;
    let t = run_tile(net, &weights, &p, &tex, 0, 0).unwrap();
    assert!(t.grid.cells.iter().all(|c| *c == 0.25));
    assert_eq!(t.grid.cells, t.sharp.cells);
    for px in &t.pixels {
        assert!(
            px.rgba[..3].iter().all(|c| (c - 0.25).abs() < 1e-6),
            "{:?}",
            px.rgba
        );
        assert_eq!(px.rgba[3], 1.0);
    }
}
