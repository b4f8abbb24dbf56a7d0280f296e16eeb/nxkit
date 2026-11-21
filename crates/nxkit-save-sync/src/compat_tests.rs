//! Byte compatibility with `save_sync.c` of the old engine. `fixtures/dm.dmsync` was packed by `ss_pack` of
//! `dm/host/save_sync.c` (NX2Apple `engine/dm`, 2026-11-21) on a Mac (`Mac14,6`) from the tree that [`tree`] builds,
//! with the title `0100abc`. This crate must unpack it to the same tree and pack the same tree to the same bytes.

// The lint does not see `cfg(all(test, ..))` as a test module.
#![allow(clippy::unwrap_used, reason = "test helpers")]

use crate::format::HDR;
use crate::{pack, sys, unpack};
use std::fs::{File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const FIXTURE: &[u8] = include_bytes!("../fixtures/dm.dmsync");
const HASH: &str = "b11af218746a9864e2940a6e9c794137";

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxkit-save-sync-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn set_mtime(p: &Path, t: u64) {
    let at = UNIX_EPOCH + Duration::from_secs(t);
    File::open(p)
        .unwrap()
        .set_times(FileTimes::new().set_accessed(at).set_modified(at))
        .unwrap();
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The tree of the generator: a file, a folder with a guest name holding a file with a guest name, an empty folder,
/// Finder's `.DS_Store` (not part of a save) and a device save.
fn tree(root: &Path) -> PathBuf {
    let save = root.join("save");
    for d in ["save/dir", "save/empty", "save_device"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(save.join("a.dat"), "hello").unwrap();
    std::fs::write(save.join(".DS_Store"), "junk").unwrap();
    std::fs::write(save.join("dir/b.bin"), "xy").unwrap();
    std::fs::write(root.join("save_device/dev.dat"), "dev").unwrap();
    sys::set_guest_name(&save.join("dir/b.bin"), b"B").unwrap();
    sys::set_guest_name(&save.join("dir"), b"Dir").unwrap();
    set_mtime(&save.join("a.dat"), 1_000_000_000);
    set_mtime(&save.join("dir/b.bin"), 1_000_000_100);
    set_mtime(&root.join("save_device/dev.dat"), 1_000_000_200);
    set_mtime(&save.join("dir"), 1_000_000_050);
    set_mtime(&save.join("empty"), 1_000_000_060);
    save
}

#[test]
fn a_save_packed_by_the_old_engine_unpacks_whole() {
    let d = scratch("dm-unpack");
    let file = d.join("dm.dmsync");
    std::fs::write(&file, FIXTURE).unwrap();
    let m = unpack::read(&file, Some(&d.join("into"))).unwrap();
    assert_eq!(
        (m.hash.as_str(), m.files, m.bytes, m.saved),
        (HASH, 3, 10, 1_000_000_200)
    );
    assert_eq!(
        (m.title.as_str(), m.device.as_str()),
        ("0100ABC", "Mac14,6")
    );
    let save = d.join("into/save");
    assert_eq!(std::fs::read(save.join("a.dat")).unwrap(), b"hello");
    assert_eq!(std::fs::read(save.join("dir/b.bin")).unwrap(), b"xy");
    assert_eq!(
        std::fs::read(d.join("into/save_device/dev.dat")).unwrap(),
        b"dev"
    );
    assert_eq!(sys::guest_name(&save.join("dir/b.bin")), b"B");
    assert_eq!(sys::guest_name(&save.join("dir")), b"Dir");
    assert!(save.join("empty").is_dir() && !save.join(".DS_Store").exists());
    assert_eq!(mtime(&save.join("dir/b.bin")), 1_000_000_100);
    assert_eq!(
        pack::scan(&save).unwrap().hash,
        HASH,
        "the unpacked save has the hash of the original"
    );
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn the_same_tree_packs_to_the_bytes_of_the_old_engine() {
    let d = scratch("dm-pack");
    let save = tree(&d);
    let out = d.join("rust.dmsync");
    let m = pack::pack_as(&save, "0100abc", "Mac14,6", &out).unwrap();
    assert_eq!(m.hash, HASH);
    assert_eq!(std::fs::read(&out).unwrap(), FIXTURE);
    // Another device changes the device field of the header and nothing else.
    pack::pack(&save, "0100abc", &out).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(bytes.len(), FIXTURE.len());
    assert_eq!(
        (&bytes[..80], &bytes[HDR..]),
        (&FIXTURE[..80], &FIXTURE[HDR..])
    );
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn a_damaged_or_cut_off_file_is_refused() {
    let d = scratch("dm-damaged");
    let file = d.join("x.dmsync");
    let n = FIXTURE.len();
    let mut flipped = FIXTURE.to_vec();
    flipped[n - 12] ^= 1;
    let mut long = FIXTURE.to_vec();
    long.push(0);
    for bytes in [&FIXTURE[..n - 1], &long[..], &flipped[..], &FIXTURE[..HDR]] {
        std::fs::write(&file, bytes).unwrap();
        assert!(unpack::read(&file, None).is_err());
    }
    std::fs::remove_dir_all(d).unwrap();
}
