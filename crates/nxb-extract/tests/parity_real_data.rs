//! Differential test on real data (ignored by default, game files and keys stay on the machine):
//!
//! ```sh
//! NXB_PARITY_BASE=<base.nsp> NXB_PARITY_UPDATE=<update.nsp> NXB_PARITY_KEYS=<prod.keys> \
//! NXB_PARITY_GAME=<profile game.tsv> NXB_PARITY_REF=<work dir of a Python build> NXB_TEST_TMP=<scratch> \
//!   cargo test -p nxb-extract --release --test parity_real_data -- --ignored
//! ```
//!
//! Extracts both versions with the split fused and compares every output with the Python build's work dir:
//! `extract/<v>/{RomFS.img, control.nacp, icon.jpg, ExeFS/*}`, `shader/pkg/*.bin`, `shader/pso_pairs.tsv`.

use std::io::Read;
use std::path::{Path, PathBuf};

fn same(a: &Path, b: &Path) -> bool {
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let (na, nb) = (
            fa.read(&mut ba).unwrap_or(usize::MAX),
            fb.read(&mut bb).unwrap_or(usize::MAX),
        );
        if na != nb || na == usize::MAX || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

#[test]
#[ignore]
fn reference_extract_and_split_match_python() {
    let env = |k: &str| std::env::var(k).ok().map(PathBuf::from);
    let (Some(base), Some(update), Some(keys), Some(game), Some(reference)) = (
        env("NXB_PARITY_BASE"),
        env("NXB_PARITY_UPDATE"),
        env("NXB_PARITY_KEYS"),
        env("NXB_PARITY_GAME"),
        env("NXB_PARITY_REF"),
    ) else {
        return;
    };
    let tmp = env("NXB_TEST_TMP")
        .unwrap_or_else(std::env::temp_dir)
        .join("nxb-extract-parity");
    let _ = std::fs::remove_dir_all(&tmp);
    let up_name = std::fs::read_dir(reference.join("extract"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n != "v0")
        .unwrap_or_default();
    let outputs = nxb_extract::split::SplitOutputs {
        pkg: tmp.join("pkg"),
        map: tmp.join("shader_map.tsv"),
        facts: tmp.join("shader_facts.tsv"),
        pairs: tmp.join("shader_pairs.tsv"),
        pso_pairs: tmp.join("pso_pairs.tsv"),
    };
    let req = nxb_extract::Request {
        keys,
        base,
        out_base: tmp.join("v0"),
        updates: vec![(update, tmp.join(&up_name))],
        threads: 8,
        mem_limit: 1 << 30,
        cache_chunks: 512,
        digests: None,
        html: false,
        base_outputs: true,
        split: Some((game, outputs)),
        app: None,
        program: None,
        discard_images: false,
        allow_damaged: false,
        manifest: false,
        progress: None,
        empty_romfs: false,
    };
    assert!(nxb_extract::extract(&req).is_ok());
    let mut checked = 0;
    for v in ["v0", up_name.as_str()] {
        for f in [
            "RomFS.img",
            "control.nacp",
            "icon.jpg",
            "ExeFS/main",
            "ExeFS/main.npdm",
            "ExeFS/rtld",
            "ExeFS/sdk",
        ] {
            assert!(
                same(
                    &tmp.join(v).join(f),
                    &reference.join("extract").join(v).join(f)
                ),
                "{v}/{f}"
            );
            checked += 1;
        }
    }
    for e in std::fs::read_dir(reference.join("shader/pkg"))
        .into_iter()
        .flatten()
        .flatten()
    {
        assert!(
            same(&tmp.join("pkg").join(e.file_name()), &e.path()),
            "{:?}",
            e.file_name()
        );
        checked += 1;
    }
    assert!(same(
        &tmp.join("pso_pairs.tsv"),
        &reference.join("shader/pso_pairs.tsv")
    ));
    assert!(checked > 14);
    let _ = std::fs::remove_dir_all(&tmp);
}
