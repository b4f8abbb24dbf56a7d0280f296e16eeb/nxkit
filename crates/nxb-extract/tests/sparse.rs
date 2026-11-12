//! A sparse base program read through its update: the base NCA stores only the ranges of its RomFS section that the
//! update does not replace, a bucket tree in the NCA maps them, and the update's BKTR tables patch the rest.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::*;
use nxb_extract::{ExtractError, Request};

#[path = "common/sparse.rs"]
mod sparse;
use sparse::fixture;

fn request(keys: &std::path::Path, base: &std::path::Path, out: &std::path::Path) -> Request {
    Request {
        keys: keys.to_path_buf(),
        base: base.to_path_buf(),
        out_base: out.to_path_buf(),
        threads: 2,
        mem_limit: 1 << 28,
        cache_chunks: 16,
        ..Request::default()
    }
}

#[test]
fn a_sparse_base_is_read_through_its_update() {
    let f = fixture();
    let dir = tmp("sparse-update");
    let keys = write_keys(&dir);
    let (base, upd) = (dir.join("base.nsp"), dir.join("upd.nsp"));
    std::fs::write(&base, &f.base).unwrap();
    std::fs::write(&upd, &f.update).unwrap();
    let mut req = request(&keys, &base, &dir.join("v0"));
    req.updates = vec![(upd.clone(), dir.join("v1"))];
    req.base_outputs = false;
    nxb_extract::extract(&req).unwrap();
    let img = std::fs::read(dir.join("v1/RomFS.img")).unwrap();
    assert_eq!(read_image(&img), f.image);
    assert_eq!(
        std::fs::read(dir.join("v1/ExeFS/main")).unwrap(),
        vec![4u8; 0x500]
    );
    cleanup(&dir);
}

#[test]
fn a_sparse_base_with_its_own_outputs_is_refused() {
    let f = fixture();
    let dir = tmp("sparse-own");
    let keys = write_keys(&dir);
    let (base, upd) = (dir.join("base.nsp"), dir.join("upd.nsp"));
    std::fs::write(&base, &f.base).unwrap();
    std::fs::write(&upd, &f.update).unwrap();
    let mut req = request(&keys, &base, &dir.join("v0"));
    req.updates = vec![(upd.clone(), dir.join("v1"))];
    let r = nxb_extract::extract(&req);
    assert!(matches!(r, Err(ExtractError::Unsupported(m)) if m.contains("sparse")));
    cleanup(&dir);
}
