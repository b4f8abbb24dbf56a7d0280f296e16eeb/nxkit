//! Tests of the plan of the movie network.

use super::*;
use crate::pack::packed_len;
use std::collections::BTreeSet;

/// A blob of the right length whose halves are small finite numbers.
fn blob() -> Vec<u8> {
    (0..BLOB_BYTES / 2)
        .flat_map(|i| (0x3000 + (i % 0x400) as u16).to_le_bytes())
        .collect()
}
fn make() -> Plan {
    plan(&blob(), &vec![0u8; RESAMPLER_BYTES]).expect("the plan builds")
}

#[test]
fn layer_offsets_follow_each_other() {
    // weights then bias, each layer padded to 0x100 bytes, the last bias 0x20 bytes: the blob ends at 0x3c320
    let mut end = 0usize;
    for (i, l) in LAYERS.iter().enumerate() {
        assert_eq!(l.w_off, end, "layer {}", i + 1);
        assert_eq!(l.b_off, l.w_off + l.weight_bytes(), "layer {}", i + 1);
        end = (l.b_off + l.cout * 2).next_multiple_of(0x100);
    }
    // the last bias is padded to 0x20 bytes only
    assert_eq!(LAYERS[11].b_off + 0x20, BLOB_BYTES);
}

#[test]
fn wrong_lengths_are_errors() {
    assert!(plan(&blob()[..100], &vec![0; RESAMPLER_BYTES]).is_err());
    assert!(plan(&blob(), &[0; 10]).is_err());
}

#[test]
fn buffers_match_the_layer_table() {
    let p = make();
    assert_eq!(p.buffers.len(), 22);
    for (i, l) in LAYERS.iter().enumerate() {
        let b = &p.buffers[i];
        assert_eq!(
            b.bytes as usize,
            packed_len(l.taps(), l.cin, l.cout),
            "{}",
            l.label
        );
        assert_eq!(b.init.as_ref().map(Vec::len), Some(b.bytes as usize));
    }
    for (i, b) in p.buffers.iter().enumerate().skip(12) {
        assert_eq!(
            b.init.as_ref().map(Vec::len).is_some(),
            i == B_RS_W,
            "{}",
            b.name
        );
    }
    let mut names = BTreeSet::new();
    for b in &p.buffers {
        assert!(names.insert(b.name), "duplicate name {}", b.name);
    }
}

/// The step that runs layer `n` (0-based) and the layers it covers: 1 + 2 and 11 + 12 are fused. Step 0 is the colour
/// conversion.
fn step_of(n: usize) -> usize {
    match n {
        0 | 1 => 1,
        2..=9 => n,
        _ => 10,
    }
}

#[test]
fn every_binding_names_a_buffer_of_the_right_size() {
    let p = make();
    assert_eq!(p.steps.len(), 13);
    assert_eq!(p.steps[0].kernel, "ndve_convert");
    assert_eq!(p.steps[1].kernel, "ndve_l01_l02");
    assert_eq!(p.steps[10].kernel, "ndve_l11_l12");
    for (n, l) in LAYERS.iter().enumerate() {
        let s = &p.steps[step_of(n)];
        if !fused_second(n) && !matches!(l.kind, Kind::First | Kind::Last) {
            assert_eq!(s.kernel, l.kernel);
        }
        for b in s.buffers.iter().chain(&s.textures) {
            if let Source::Buffer(i) = b.source {
                assert!(i < p.buffers.len(), "layer {} binds buffer {i}", n + 1);
            }
        }
        let size_of = |index: u32| -> Option<u64> {
            s.buffers
                .iter()
                .find(|b| b.index == index)
                .and_then(|b| match b.source {
                    Source::Buffer(i) => Some(p.buffers[i].bytes),
                    Source::Game(_) => None,
                })
        };
        let weights = |l: &Layer| Some(packed_len(l.taps(), l.cin, l.cout) as u64);
        match l.kind {
            Kind::First => {
                // writes layer 2's output, has the weights of layers 1 and 2
                assert_eq!(size_of(1), Some(LAYERS[1].out_bytes()));
                assert_eq!(size_of(2), weights(l));
                assert_eq!(size_of(5), weights(&LAYERS[1]));
            }
            Kind::Last => {
                // reads layer 10's output (layer 11's input), has the weights of layers 11 and 12
                assert_eq!(size_of(0), Some(LAYERS[10].in_bytes()));
                assert_eq!(size_of(2), weights(&LAYERS[10]));
                assert_eq!(size_of(5), weights(l));
                assert_eq!(size_of(1), None);
            }
            Kind::Conv3 | Kind::Down | Kind::Up if fused_second(n) => {}
            Kind::Conv3 | Kind::Down | Kind::Up => {
                assert_eq!(size_of(0), Some(l.in_bytes()), "layer {} input", n + 1);
                assert_eq!(size_of(1), Some(l.out_bytes()), "layer {} output", n + 1);
                assert_eq!(size_of(2), weights(l));
                if l.kind == Kind::Up {
                    // the skip has the output's size
                    assert_eq!(size_of(3), Some(l.out_bytes()), "layer {} skip", n + 1);
                } else {
                    assert_eq!(size_of(3), None);
                }
            }
        }
    }
}

/// The routes keep every tensor alive until its last reader: layer 2's output is read by layers 3 and 10, layer 10's
/// output only by the fused 11 + 12, and the fused steps do not write the intermediate tensors.
#[test]
fn fused_steps_route_the_tensors() {
    let p = make();
    let writes = |s: &Step| -> Vec<Source> {
        s.buffers
            .iter()
            .filter(|b| b.writes)
            .map(|b| b.source)
            .collect()
    };
    assert_eq!(writes(&p.steps[1]), vec![Source::Buffer(B_S16)]);
    assert!(
        p.steps[2]
            .buffers
            .iter()
            .any(|b| b.index == 0 && b.source == Source::Buffer(B_S16))
    );
    // layer 10 (step 9) reads s16 as the skip and writes t1, which the last fused step reads
    assert!(
        p.steps[9]
            .buffers
            .iter()
            .any(|b| b.index == 3 && b.source == Source::Buffer(B_S16))
    );
    assert_eq!(writes(&p.steps[9]), vec![Source::Buffer(B_T1)]);
    assert!(
        p.steps[10]
            .buffers
            .iter()
            .any(|b| b.index == 0 && b.source == Source::Buffer(B_T1))
    );
    assert!(writes(&p.steps[10]).is_empty());
}

#[test]
fn the_grids_cover_the_images() {
    for l in &LAYERS {
        let g = l.groups();
        match l.geo {
            Geometry::Tiles { tw, th } | Geometry::Pair { tw, th } => {
                assert!(
                    g[0] as usize * tw >= l.out_wh.0 && g[1] as usize * th >= l.out_wh.1,
                    "{}",
                    l.kernel
                );
                // the kernels read their tile geometry from template arguments; the widths divide the images
                assert_eq!(l.out_wh.0 % tw, 0, "{}", l.kernel);
            }
            Geometry::Tasks { mt } => {
                let (w, h) = if l.kind == Kind::Up {
                    l.in_wh
                } else {
                    l.out_wh
                };
                assert_eq!(w % (8 * mt), 0, "{}", l.kernel);
                assert!(g[0] as usize * 4 >= w / (8 * mt) * h, "{}", l.kernel);
            }
        }
    }
    let p = make();
    let rs = &p.steps[12];
    // 10 threadgroups x 2 rounds x 8 simdgroups x 2 blocks = 320 blocks of 8 pixels = 2560; 180 rows of blocks = 1440
    assert_eq!(rs.groups[0] * 2 * 8 * 2 * 8, 2560);
    assert_eq!(rs.threads[0], 8 * 32);
    assert_eq!(rs.groups[1] * 8, 1440);
    let luma = &p.steps[11];
    assert_eq!(
        (
            luma.groups[0] * luma.threads[0],
            luma.groups[1] * luma.threads[1]
        ),
        (1920, 1080)
    );
}

/// The text of the kernel's parameter list in the MSL.
fn signature(name: &str) -> String {
    let src = crate::msl::SOURCE;
    let head = format!("kernel void {name}(");
    let start = src
        .find(&head)
        .unwrap_or_else(|| panic!("no kernel {name}"))
        + head.len();
    let end = src[start..].find(") {").map_or(src.len(), |e| start + e);
    src[start..end].to_string()
}
fn indices(sig: &str, attr: &str) -> BTreeSet<u32> {
    let key = format!("[[{attr}(");
    sig.match_indices(&key)
        .filter_map(|(i, _)| {
            let rest = &sig[i + key.len()..];
            rest[..rest.find(')')?].parse().ok()
        })
        .collect()
}

#[test]
fn bindings_are_the_kernels_arguments() {
    let p = make();
    for s in &p.steps {
        let sig = signature(s.kernel);
        let b: BTreeSet<u32> = s.buffers.iter().map(|b| b.index).collect();
        let t: BTreeSet<u32> = s.textures.iter().map(|b| b.index).collect();
        assert_eq!(indices(&sig, "buffer"), b, "{} buffers", s.kernel);
        assert_eq!(indices(&sig, "texture"), t, "{} textures", s.kernel);
        assert_eq!(
            b.len(),
            s.buffers.len(),
            "{}: a buffer index twice",
            s.kernel
        );
        assert_eq!(
            t.len(),
            s.textures.len(),
            "{}: a texture index twice",
            s.kernel
        );
        let threads = s.threads[0] * s.threads[1] * s.threads[2];
        assert!(
            threads <= 1024 && s.groups.iter().all(|&g| g > 0),
            "{}",
            s.kernel
        );
    }
}

#[test]
fn no_step_reads_and_writes_one_buffer() {
    let p = make();
    for s in &p.steps {
        let reads: Vec<Source> = s
            .buffers
            .iter()
            .filter(|b| !b.writes)
            .map(|b| b.source)
            .collect();
        for w in s.buffers.iter().filter(|b| b.writes) {
            assert!(
                !reads.contains(&w.source),
                "{} reads and writes one buffer",
                s.kernel
            );
        }
    }
}

/// The constants of the fused pairs in the MSL are those of this module.
#[test]
fn the_pair_constants_agree_with_the_msl() {
    let src = crate::msl::SOURCE;
    for (name, v) in [("TW", PAIR_TW), ("TH", PAIR_TH)] {
        assert!(
            src.contains(&format!("static constant constexpr int {name} = {v};")),
            "Pair::{name}"
        );
    }
    assert!(src.contains(&format!("THREADS = {};", PAIR_SIMDGROUPS * 32)));
    assert!(src.contains(&format!(
        "for (int r = int(sg); r < P::TH; r += {PAIR_SIMDGROUPS})"
    )));
    assert_eq!(THREADS as usize, PAIR_SIMDGROUPS * 32);
}

/// The fused tiling on the CPU: the geometry of `Pair` (`conv.metal`) visits every pixel of every tensor exactly once
/// where it must, and no read leaves the staged tiles.
#[test]
fn the_fused_tiling_covers_every_pixel_once() {
    let (w, h) = (960usize, 540usize);
    let (tw, th, sgs) = (PAIR_TW, PAIR_TH, PAIR_SIMDGROUPS);
    let (iw, ih, mw, mh) = (tw + 4, th + 4, tw + 2, th + 2);
    let last_x = tw - 8;
    let g = LAYERS[0].groups();
    assert_eq!((g[0] as usize, g[1] as usize), (w / tw, h / th));
    assert_eq!(g, LAYERS[11].groups());
    assert_eq!((w % tw, h % th), (0, 0), "the tiles are exact");
    // the mid tile is 4 m-tiles of 8 wide, the staged input covers the taps of all of them
    assert_eq!(mw, 32);
    assert!(
        24 + 7 + 2 < iw,
        "the first layer's taps stay in the staged input"
    );
    assert!(
        last_x + 7 + 2 < mw,
        "the second layer's taps stay in the mid tile"
    );
    assert!(mh + 2 == ih);

    // rows: every row of the mid tile and of the output tile belongs to one simdgroup
    for rows in [mh, th] {
        let mut owner = vec![0u32; rows];
        for sg in 0..sgs {
            for b in 0..rows.div_ceil(sgs) {
                let r = sg + sgs * b;
                if r < rows {
                    owner[r] += 1;
                }
            }
        }
        assert!(owner.iter().all(|&c| c == 1));
    }
    // the output row: the four m-tiles start at min(8 i, last_x); each pixel belongs to the first tile that holds it
    let mut hit = vec![0u32; tw];
    let mut written = vec![0u32; tw];
    for i in 0..4 {
        let x = (8 * i).min(last_x);
        for (k, wr) in written.iter_mut().enumerate().skip(x).take(8) {
            *wr += 1;
            assert!(
                x + 8 <= tw,
                "a tile never writes into the next threadgroup's pixels"
            );
            if k >= 8 * i {
                hit[k] += 1;
            }
        }
    }
    assert!(hit.iter().all(|&c| c == 1), "{hit:?}");
    assert!(written.iter().all(|&c| (1..=2).contains(&c)));
    // the whole image: every output pixel is produced by exactly one threadgroup
    let mut cover = vec![0u8; w * h];
    for ty in 0..h / th {
        for tx in 0..w / tw {
            for r in 0..th {
                for x in 0..tw {
                    cover[(ty * th + r) * w + tx * tw + x] += 1;
                }
            }
        }
    }
    assert!(cover.iter().all(|&c| c == 1));
    // the border pixels cleared in the mid tile are exactly those outside the image
    let (gx, gy) = (w / tw, h / th);
    for ty in [0, gy - 1, 1] {
        for tx in [0, gx - 1, 1] {
            for cy in 0..mh {
                for cx in 0..mw {
                    let (x, y) = ((tx * tw + cx) as i64 - 1, (ty * th + cy) as i64 - 1);
                    let outside = x < 0 || y < 0 || x >= w as i64 || y >= h as i64;
                    let cleared = (tx == 0 && cx == 0)
                        || (tx == gx - 1 && cx == mw - 1)
                        || (ty == 0 && cy == 0)
                        || (ty == gy - 1 && cy == mh - 1);
                    assert_eq!(outside, cleared, "tile ({tx}, {ty}) mid pixel ({cx}, {cy})");
                }
            }
        }
    }
}
