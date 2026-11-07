//! `nxb-split --game <game.tsv> --pkg <dir> --map <tsv> --facts <tsv> --pairs <tsv> --pso-pairs <tsv> [--threads N] <version dir>...`

use std::path::PathBuf;

/// `nxb split` / `nxb-split`: returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let a = args;
    let mut opt = std::collections::BTreeMap::new();
    let mut versions = Vec::new();
    let mut i = 0;
    while i < a.len() {
        if let Some(k) = a[i].strip_prefix("--") {
            opt.insert(k.to_string(), a.get(i + 1).cloned().unwrap_or_default());
            i += 2;
        } else {
            versions.push(PathBuf::from(&a[i]));
            i += 1;
        }
    }
    let get = |k: &str| opt.get(k).map(PathBuf::from);
    let (Some(game_tsv), Some(pkg), Some(map), Some(facts), Some(pairs), Some(pso_pairs)) = (
        get("game"),
        get("pkg"),
        get("map"),
        get("facts"),
        get("pairs"),
        get("pso-pairs"),
    ) else {
        eprintln!(
            "usage: nxb-split --game <game.tsv> --pkg <dir> --map <tsv> --facts <tsv> --pairs <tsv> --pso-pairs <tsv> [--threads N] <version dir>..."
        );
        return 2;
    };
    let threads = opt
        .get("threads")
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        });
    let outputs = crate::split::SplitOutputs {
        pkg,
        map,
        facts,
        pairs,
        pso_pairs,
    };
    let req = crate::split::SplitRequest {
        versions,
        game_tsv,
        outputs,
        threads,
        nro_dir: None,
        oodle: None,
        progress: None,
    };
    let t = std::time::Instant::now();
    match crate::split::split(&req) {
        Ok((_, bad)) => {
            for b in bad {
                println!("{b}");
            }
            eprintln!("nxb-split: {:.2} s", t.elapsed().as_secs_f64());
            0
        }
        Err(e) => {
            println!("[RED] {e}");
            i32::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}
