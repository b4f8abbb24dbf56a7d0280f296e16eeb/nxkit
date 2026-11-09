//! The extract command line: `--keys <prod.keys> --base <nsp> --out <dir> [--update <nsp> <dir>]... [--threads N]
//! [--digests <tsv>]`, or `--nup <game.nup> [--work <dir>] [--version N] [--threads N] [--mem-mib N] [--digests <tsv>]
//! [--discard-images] [--no-images]` (a Switch 2 package: no keys).

use std::path::PathBuf;

fn usage() -> i32 {
    eprintln!(
        "usage: nxb-extract --keys <prod.keys> --base <base.nsp> --out <dir> [--update <update.nsp> <dir>]... [--threads N] [--mem-mib N] [--cache-mib N] [--digests <tsv>] [--allow-damaged] [--manifest] | --keys <prod.keys> --aoc <dlc.nsp> [<title ID>] --out <dir>"
    );
    2
}

/// `nxb extract` / `nxb-extract`: returns the exit code. A `.nup` (`--nup <file>`, or the only argument) goes to
/// [`crate::nup`]; everything else is the NSP extract.
pub fn main(args: &[String]) -> i32 {
    let a: Vec<String> = args.to_vec();
    if let Some(code) = nup_main(&a) {
        return code;
    }
    if a.len() == 3 && a[1] == "--contents" {
        return match crate::run::contents(std::path::Path::new(&a[0]), std::path::Path::new(&a[2]))
        {
            Ok(t) => {
                print!("{t}");
                0
            }
            Err(e) => {
                println!("[RED] {e}");
                e.exit_code()
            }
        };
    }
    let (mut keys, mut base, mut out, mut digests) = (None, None, None, None);
    let mut updates = Vec::new();
    let mut threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let (mut mem, mut cache) = (1024u64, 512usize);
    let mut html = false;
    let mut app = None;
    let mut program = None;
    let mut discard_images = false;
    let (mut allow_damaged, mut manifest) = (false, false);
    let mut aoc: Option<(PathBuf, Option<u64>)> = None;
    let mut base_outputs = true;
    let mut split: std::collections::BTreeMap<String, PathBuf> = std::collections::BTreeMap::new();
    let mut i = 0;
    while i < a.len() {
        let next = |k: usize| a.get(k).cloned();
        match a[i].as_str() {
            "--keys" => keys = next(i + 1).map(PathBuf::from),
            "--base" => base = next(i + 1).map(PathBuf::from),
            "--out" => out = next(i + 1).map(PathBuf::from),
            "--app" => match next(i + 1).and_then(|v| u64::from_str_radix(&v, 16).ok()) {
                Some(v) => app = Some(v),
                None => return usage(),
            },
            "--program" => match next(i + 1).and_then(|v| u64::from_str_radix(&v, 16).ok()) {
                Some(v) => program = Some(v),
                None => return usage(),
            },
            "--digests" => digests = next(i + 1).map(PathBuf::from),
            "--threads" => threads = next(i + 1).and_then(|v| v.parse().ok()).unwrap_or(threads),
            "--mem-mib" => mem = next(i + 1).and_then(|v| v.parse().ok()).unwrap_or(mem),
            "--cache-mib" => cache = next(i + 1).and_then(|v| v.parse().ok()).unwrap_or(cache),
            k @ ("--split-game" | "--split-pkg" | "--split-map" | "--split-facts"
            | "--split-pairs" | "--split-pso-pairs") => {
                if let Some(v) = next(i + 1) {
                    split.insert(
                        k.trim_start_matches("--split-").to_string(),
                        PathBuf::from(v),
                    );
                }
            }
            "--discard-images" => {
                discard_images = true;
                i += 1;
                continue;
            }
            "--allow-damaged" => {
                allow_damaged = true;
                i += 1;
                continue;
            }
            "--manifest" => {
                manifest = true;
                i += 1;
                continue;
            }
            // `--aoc <dlc.nsp> [<DLC title ID>]`: the RomFS of an add-on content package as `<out>/RomFS.img`
            "--aoc" => {
                let Some(p) = next(i + 1) else {
                    return usage();
                };
                let id = a
                    .get(i + 2)
                    .filter(|x| !x.starts_with("--"))
                    .and_then(|x| u64::from_str_radix(x, 16).ok());
                aoc = Some((PathBuf::from(p), id));
                i += if id.is_some() { 2 } else { 1 };
            }
            "--updates-only" => {
                base_outputs = false;
                i += 1;
                continue;
            }
            "--html" => {
                html = true;
                i += 1;
                continue;
            }
            "--update" => match (next(i + 1), next(i + 2)) {
                (Some(u), Some(d)) => {
                    updates.push((PathBuf::from(u), PathBuf::from(d)));
                    i += 1;
                }
                _ => return usage(),
            },
            _ => return usage(),
        }
        i += 2;
    }
    if let Some((pkg, id)) = aoc {
        let (Some(keys), Some(out_base)) = (keys, out) else {
            return usage();
        };
        let req = crate::Request {
            keys,
            base: pkg,
            out_base,
            threads,
            mem_limit: mem << 20,
            cache_chunks: cache,
            discard_images,
            allow_damaged,
            ..crate::Request::default()
        };
        return match crate::aoc::aoc(&req, id) {
            Ok(s) => {
                s.closing.wait();
                0
            }
            Err(e) => {
                println!("[RED] {e}");
                e.exit_code()
            }
        };
    }
    let (Some(keys), Some(base), Some(out_base)) = (keys, base, out) else {
        return usage();
    };
    let g = |k: &str| split.get(k).cloned();
    let split = match (
        g("game"),
        g("pkg"),
        g("map"),
        g("facts"),
        g("pairs"),
        g("pso-pairs"),
    ) {
        (Some(game), Some(pkg), Some(map), Some(facts), Some(pairs), Some(pso_pairs)) => Some((
            game,
            crate::split::SplitOutputs {
                pkg,
                map,
                facts,
                pairs,
                pso_pairs,
            },
        )),
        _ if split.is_empty() => None,
        _ => return usage(),
    };
    let req = crate::Request {
        keys,
        base,
        out_base,
        updates,
        threads,
        mem_limit: mem << 20,
        cache_chunks: cache,
        digests,
        html,
        base_outputs,
        split,
        app,
        program,
        discard_images,
        allow_damaged,
        manifest,
        progress: None,
    };
    match crate::extract(&req) {
        Ok(mut s) => {
            let closing = std::mem::replace(&mut s.closing, crate::write::Closing::done());
            eprintln!("nxb-extract: {}", crate::prof::summary());
            eprintln!(
                "nxb-extract: open + small files {:.2} s, images {:.2} s; decrypted {:.1} MB in {} chunks, {} cache hits",
                s.seconds.0,
                s.seconds.1,
                s.cache.produced_bytes as f64 / 1e6,
                s.cache.produced,
                s.cache.hits
            );
            let c = closing.wait();
            if c > 0.5 {
                eprintln!(
                    "nxb-extract: closing the images took {c:.1} s more (the disk taking the data)"
                );
            }
            match s.split_unsupported {
                Some(why) => {
                    let e = crate::ExtractError::Unsupported(why);
                    println!("[RED] {e}");
                    e.exit_code()
                }
                None => 0,
            }
        }
        Err(e) => {
            if let crate::ExtractError::Damaged(p) = &e {
                for x in p {
                    println!("[RED] hash check failed: {x}");
                }
            }
            println!("[RED] {e}");
            e.exit_code()
        }
    }
}

/// The NUP form of the command line, or `None` when the arguments name no `.nup`.
fn nup_main(a: &[String]) -> Option<i32> {
    let named = a
        .iter()
        .position(|x| x == "--nup")
        .and_then(|i| a.get(i + 1))
        .cloned();
    let only = (a.len() == 1 && !a[0].starts_with('-')).then(|| a[0].clone());
    let path = PathBuf::from(named.or(only)?);
    if !crate::nup::is_nup(&path) {
        println!("[RED] not a NUP: {}", path.display());
        return Some(1);
    }
    let mut r = crate::nup::NupRequest {
        nup: path,
        version: None,
        work: PathBuf::new(),
        threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4),
        mem_limit: 1 << 30,
        discard_images: false,
        digests: None,
        skip_images: false,
        progress: None,
    };
    let mut i = 0;
    while i < a.len() {
        let v = a.get(i + 1).cloned().unwrap_or_default();
        match a[i].as_str() {
            "--nup" => {}
            "--work" => r.work = PathBuf::from(&v),
            "--version" => r.version = v.parse().ok(),
            "--threads" => r.threads = v.parse().unwrap_or(r.threads),
            "--mem-mib" => r.mem_limit = v.parse::<u64>().map(|m| m << 20).unwrap_or(r.mem_limit),
            "--digests" => r.digests = Some(PathBuf::from(&v)),
            "--no-images" => {
                r.skip_images = true;
                i += 1;
                continue;
            }
            "--discard-images" => {
                r.discard_images = true;
                i += 1;
                continue;
            }
            s if !s.starts_with('-') => {
                i += 1;
                continue;
            }
            other => {
                println!("[RED] unknown option for a NUP: {other}");
                return Some(2);
            }
        }
        i += 2;
    }
    Some(match crate::nup::extract_nup(&r) {
        Ok(_) => 0,
        Err(e) => {
            println!("[RED] {e}");
            e.exit_code()
        }
    })
}
