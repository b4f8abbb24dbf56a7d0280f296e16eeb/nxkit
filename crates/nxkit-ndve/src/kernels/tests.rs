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
    assert_eq!(p.buffers.len(), 21);
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

#[test]
fn every_binding_names_a_buffer_of_the_right_size() {
    let p = make();
    assert_eq!(p.steps.len(), 14);
    for (n, (l, s)) in LAYERS.iter().zip(&p.steps).enumerate() {
        assert_eq!(s.kernel, l.kernel);
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
        match l.kind {
            Kind::First => assert_eq!(size_of(1), Some(l.out_bytes())),
            Kind::Last => assert_eq!(size_of(0), Some(l.in_bytes())),
            Kind::Conv3 | Kind::Down | Kind::Up => {
                assert_eq!(size_of(0), Some(l.in_bytes()), "layer {} input", n + 1);
                assert_eq!(size_of(1), Some(l.out_bytes()), "layer {} output", n + 1);
                if l.kind == Kind::Up {
                    // the skip has the output's size
                    assert_eq!(size_of(3), Some(l.out_bytes()), "layer {} skip", n + 1);
                } else {
                    assert_eq!(size_of(3), None);
                }
            }
        }
        assert_eq!(size_of(2), Some(packed_len(l.taps(), l.cin, l.cout) as u64));
    }
}

#[test]
fn the_grids_cover_the_images() {
    for l in &LAYERS {
        let g = l.groups();
        match l.geo {
            Geometry::Tiles { tw, th } => {
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
    let rs = &p.steps[13];
    // 8 threadgroups x 5 rounds x 4 simdgroups x 2 blocks = 320 blocks of 8 pixels = 2560; 180 rows of blocks = 1440
    assert_eq!(rs.groups[0] * 5 * 4 * 2 * 8, 2560);
    assert_eq!(rs.groups[1] * 8, 1440);
    let luma = &p.steps[12];
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
