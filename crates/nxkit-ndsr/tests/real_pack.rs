//! Checks the table of `net.rs` against the real packs. Skipped unless `NXKIT_NDSR_PACK` names a pack file
//! (`agl_ndsr*.pck`, decompressed) or a directory that holds some of the three.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use nxb_model::{sha256, to_hex};
use nxkit_ndsr::fingerprint;
use nxkit_ndsr::net::{NETS, Pack};
use nxkit_ndsr::pack_file::PackFile;
use nxkit_ndsr::reference::{Picture, Push, run_image};

fn packs() -> Vec<(Pack, Vec<u8>)> {
    let Ok(path) = std::env::var("NXKIT_NDSR_PACK") else {
        eprintln!("NXKIT_NDSR_PACK not set: skipped");
        return Vec::new();
    };
    let path = std::path::PathBuf::from(path);
    let files: Vec<std::path::PathBuf> = if path.is_dir() {
        Pack::ALL
            .iter()
            .map(|p| path.join(p.name()))
            .filter(|p| p.exists())
            .collect()
    } else {
        vec![path]
    };
    files
        .into_iter()
        .map(|f| {
            let bytes = std::fs::read(&f).unwrap();
            let n = PackFile::parse(&bytes).unwrap().header[2];
            let pack = Pack::ALL
                .into_iter()
                .find(|p| p.entry_count() == n)
                .expect("an NDSR pack");
            (pack, bytes)
        })
        .collect()
}

#[test]
fn real_packs_match_the_table() {
    for (pack, bytes) in packs() {
        assert_eq!(bytes.len(), pack.byte_len(), "{pack:?}");
        assert_eq!(to_hex(&sha256(&bytes).0), pack.sha256(), "{pack:?}");
        let f = PackFile::parse(&bytes).unwrap();
        assert_eq!(f.header, pack.header_words(), "{pack:?}");
        assert_eq!(f.entries.len(), pack.entry_count() as usize);

        let nets: Vec<_> = NETS.iter().filter(|n| n.pack == pack).collect();
        let found = f.distinct_networks().unwrap();
        assert_eq!(found.len(), nets.len(), "{pack:?}");
        for (entry, net) in found.iter().zip(&nets) {
            assert_eq!(entry.config, net.config(), "{}", net.key);
            let blob = net.blob();
            assert_eq!(
                (entry.weights.offset, entry.weights.len),
                (blob.offset, blob.len),
                "{}",
                net.key
            );
            let w = f.data(&entry.weights);
            assert!(fingerprint::weights_ok(net, w), "{}", net.key);
            assert_eq!(to_hex(&sha256(w).0), blob.sha256);
        }
    }
}

#[test]
fn real_vr_blobs_run_through_the_reference() {
    for (pack, bytes) in packs().into_iter().filter(|(p, _)| *p == Pack::Vr) {
        let f = PackFile::parse(&bytes).unwrap();
        let tex = Picture::from_fn(32, 24, |x, y| {
            let v = ((x * 7 + y * 3) % 64) as f32 / 64.0;
            [v, 0.25 + v * 0.5, 1.0 - v, 1.0]
        });
        let push = Push {
            w_in: 32,
            h_in: 24,
            w_out: 48,
            h_out: 36,
            offset: (0, 0),
            unsharp: 0.5,
            dither: true,
        };
        for (entry, net) in f
            .distinct_networks()
            .unwrap()
            .iter()
            .zip(NETS.iter().filter(|n| n.pack == pack))
        {
            let img = run_image(net, f.data(&entry.weights), &push, &tex).unwrap();
            assert!(
                img.rgba.iter().all(|p| p.iter().all(|c| c.is_finite())),
                "{}",
                net.key
            );
        }
    }
}
