//! Checks the offsets and hashes of `docs/plans/ndve-native.md` against the real decompressed file.
//! Skipped unless `NXKIT_NDVE_DAT` names the file.

use nxb_model::{sha256, to_hex};
use nxkit_ndve::file::Network;
use nxkit_ndve::fingerprint::{self, BLOB_LEN, BLOB_SHA256, FILE_LEN, FILE_SHA256};
use nxkit_ndve::net::LAYERS;

#[test]
fn real_file_matches_the_notes() {
    let Ok(path) = std::env::var("NXKIT_NDVE_DAT") else {
        eprintln!("NXKIT_NDVE_DAT not set: skipped");
        return;
    };
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(bytes.len(), FILE_LEN);
    assert_eq!(to_hex(&sha256(&bytes).0), FILE_SHA256);

    let n = Network::parse(&bytes).unwrap();
    assert_eq!(n.pack_offset, 0x28);
    assert_eq!(n.pack_len, 0x106000);
    assert_eq!(n.models.len(), 2);
    assert_eq!(n.models[0].kind, 0x0100_0000);
    assert!((n.models[0].scale - 4.0 / 3.0).abs() < 1e-6);
    assert_eq!((n.models[1].kind, n.models[1].scale), (0x0100_0004, 1.0));
    assert_eq!(n.tables.len(), 2);
    assert_eq!(n.tables[0].offset, 0x106028);
    assert_eq!(n.tables[0].weights_offset, 0x1060c8);
    assert_eq!(n.tables[0].weights_len, BLOB_LEN);
    assert_eq!(n.tables[0].shaders.len(), 9);
    assert_eq!(n.tables[1].offset, 0x189558);
    assert_eq!(n.tables[1].weights_offset, 0x1895f8);

    let a = n.weights(&bytes, 0).unwrap();
    let b = n.weights(&bytes, 1).unwrap();
    assert_eq!(a, b, "the two networks share their weights");
    assert_eq!(to_hex(&sha256(a).0), BLOB_SHA256);
    let res = n.resampler(&bytes).unwrap();
    assert!(fingerprint::weights_ok(a, res));

    // Every layer's slices sit inside the blob, and each bias is finite halves.
    for l in &LAYERS {
        assert!(
            l.weights(a).is_some() && l.bias(a).is_some(),
            "layer {}",
            l.number
        );
    }
}
