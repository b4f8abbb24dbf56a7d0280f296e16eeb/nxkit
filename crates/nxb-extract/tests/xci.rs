//! XCI (gamecard dump) input on synthetic packages: the secure partition's NCAs give the same outputs as the same NCAs
//! in an NSP, and hostile containers are refused with an error, never a panic or a read outside the file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::*;
use nxb_extract::Request;

fn request(keys: &std::path::Path, base: &std::path::Path, out: &std::path::Path) -> Request {
    Request {
        keys: keys.to_path_buf(),
        base: base.to_path_buf(),
        out_base: out.to_path_buf(),
        updates: vec![],
        threads: 2,
        mem_limit: 1 << 28,
        cache_chunks: 16,
        digests: None,
        html: false,
        base_outputs: true,
        split: None,
        app: None,
        program: None,
        discard_images: false,
        allow_damaged: false,
        manifest: false,
        progress: None,
        empty_romfs: false,
    }
}

#[test]
fn xci_secure_partition_equals_the_same_ncas_in_an_nsp() {
    let dir = tmp("xci");
    let keys = write_keys(&dir);
    let t = title(0x80, 0x0100_0000_0000_1000, 0, 7, true);
    std::fs::write(dir.join("g.nsp"), pfs0(&t.files)).unwrap();
    // the card also has an update partition and a normal partition that are not read
    let card = xci(&[
        ("update", vec![("junk.bin".into(), vec![1; 10])]),
        ("normal", vec![]),
        ("secure", t.files.clone()),
        ("logo", vec![]),
    ]);
    std::fs::write(dir.join("g.xci"), card).unwrap();
    nxb_extract::extract(&request(&keys, &dir.join("g.nsp"), &dir.join("a"))).unwrap();
    nxb_extract::extract(&request(&keys, &dir.join("g.xci"), &dir.join("b"))).unwrap();
    same_outputs(&dir.join("a"), &dir.join("b"));
    // the engine's own extractor on the same card (only when NXB_DOTNET and NXB_NX_EXTRACT_DLL are set)
    if oracle_extract(&keys, &dir.join("g.xci"), &dir.join("c"), &[]).is_some() {
        same_outputs(&dir.join("c"), &dir.join("b"));
    }
    let img = std::fs::read(dir.join("b/RomFS.img")).unwrap();
    let files = read_image(&img);
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].0, "/game.bin");
    assert_eq!(files[0].1, vec![7 ^ 0x55; 0x9000]);
    cleanup(&dir);
}

#[test]
fn hostile_cards_are_refused() {
    let dir = tmp("xci-bad");
    let keys = write_keys(&dir);
    let t = title(0x80, 0x0100_0000_0000_1000, 0, 7, true);
    let good = xci(&[("secure", t.files.clone())]);
    let run = |name: &str, bytes: Vec<u8>| {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        nxb_extract::extract(&request(&keys, &p, &dir.join("o"))).map(|_| ())
    };
    assert!(run("ok.xci", good.clone()).is_ok());
    // no secure partition
    assert!(run("nosecure.xci", xci(&[("normal", t.files.clone())])).is_err());
    // the root offset outside the file, and zero
    for at in [good.len() as u64 + 1, u64::MAX, 0] {
        let mut b = good.clone();
        b[0x130..0x138].copy_from_slice(&at.to_le_bytes());
        assert!(run("root.xci", b).is_err());
    }
    // a truncated card: the partition runs past the end
    let cut = good[..good.len() - 100].to_vec();
    assert!(run("cut.xci", cut).is_err());
    // an entry with a huge offset and size, a name offset outside the table, a huge entry count
    let root = 0xF000usize;
    for (at, v) in [(0x10usize, u64::MAX), (0x18, u64::MAX), (0x20, 0xFFFF_FFFF)] {
        let mut b = good.clone();
        b[root + at..root + at + 8].copy_from_slice(&v.to_le_bytes());
        assert!(run("entry.xci", b).is_err());
    }
    let mut b = good.clone();
    b[root + 4..root + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(run("count.xci", b).is_err());
    // a card shorter than its header
    assert!(run("tiny.xci", vec![0; 0x105]).is_err());
    cleanup(&dir);
}
