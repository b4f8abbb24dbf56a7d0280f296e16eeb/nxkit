//! `shaderbuild build <pkg> <out> --profile D --chain V --game ID --inputs DIGEST --store D --work D
//! [--jobs N (default: the P-core equivalents)] [--batch N] [--batch-first N] [--scratch D] [--own-front off|VERDICTS [--own-allow V,..]] [key...]`

use nxb_stages::build::{BuildConfig, build};
use std::io::Write;
use std::path::PathBuf;

pub(crate) fn main(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let mut pos = Vec::new();
    let mut opt = std::collections::BTreeMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--") {
            opt.insert(name.to_string(), it.next().cloned().unwrap_or_default());
        } else {
            pos.push(a.clone());
        }
    }
    let get = |k: &str| opt.get(k).cloned().unwrap_or_default();
    if pos.len() < 2 {
        let _ = writeln!(
            err,
            "usage: nxb shaders build <pkg> <out> --profile D --chain V --game ID --inputs DIGEST --store D --work D [--jobs N (default: the P-core equivalents)] [--batch N] [--batch-first N] [--scratch D] [--own-front off|VERDICTS [--own-allow V,..]] [key...]"
        );
        return 2;
    }
    let cfg = BuildConfig {
        pkg: PathBuf::from(&pos[0]),
        out: PathBuf::from(&pos[1]),
        profile: get("profile").into(),
        chain: get("chain"),
        game: get("game"),
        inputs: get("inputs"),
        store: get("store").into(),
        work: get("work").into(),
        scratch: opt.get("scratch").map_or_else(
            || std::env::temp_dir().join(format!("nxb-build-{}", std::process::id())),
            PathBuf::from,
        ),
        jobs: get("jobs")
            .parse()
            .unwrap_or_else(|_| nxb_sched::Budget::detect(nxb_sched::DEFAULT_E_FACTOR, 0).tokens),
        batch: get("batch").parse().unwrap_or(128),
        batch_first: get("batch-first").parse().unwrap_or(16),
        keys: pos[2..].to_vec(),
        on_done: None,
        on_start: None,
        order: Vec::new(),
        manifest: Vec::new(),
        partial: false,
        // on by default (the raw tier, and own-identical shaders when verdicts exist). `--own-front off` or NXB_OWN_FRONT=0 turns it off
        own_front: match opt.get("own-front").map(String::as_str) {
            Some("off") => None,
            v => Some(nxb_stages::own_front::OwnFrontConfig {
                verdicts: v.map_or_else(
                    || nxb_stages::own_front::default_config().verdicts,
                    PathBuf::from,
                ),
                allow: opt
                    .get("own-allow")
                    .map_or("own-identical", String::as_str)
                    .split(',')
                    .map(str::to_string)
                    .collect(),
            }),
        },
    };
    let work = cfg.work.clone();
    match build(cfg) {
        Ok(r) => {
            let ok = r.results.iter().filter(|x| x.1 == "ok").count();
            let _ = writeln!(
                out,
                "shaders {}: ok {ok}, other {}; wall {:.1} s",
                r.results.len(),
                r.results.len() - ok,
                r.wall.as_secs_f64()
            );
            let _ = writeln!(out, "translations from the store: {}", r.xlate_hits);
            for (why, n) in &r.xlate_refused {
                let _ = writeln!(out, "translator refused: {n} keys ({why})");
            }
            let _ = writeln!(out, "{}", r.own_note);
            if let Some((own, fb)) = r.own {
                let _ = writeln!(
                    out,
                    "own front end: {own} libraries, {fb} fallbacks to Apple"
                );
            }
            for (i, p) in r.phases.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "phase {i}: {} tasks, {:.1} s, utilisation {:.0} %, steals {}",
                    p.tasks_run,
                    p.wall.as_secs_f64(),
                    100.0 * p.utilisation(),
                    p.steals
                );
            }
            let mut tsv = String::from("what\tkey\tstart_ms\twall_ms\n");
            for s in &r.spans {
                tsv.push_str(&format!(
                    "{}\t{}\t{}\t{}\n",
                    s.what,
                    s.key,
                    s.start.as_millis(),
                    s.wall.as_millis()
                ));
            }
            let _ = std::fs::write(work.join("timeline.tsv"), tsv);
            for (k, st, e) in r.results.iter().filter(|x| x.1 != "ok").take(10) {
                let _ = writeln!(out, "  {k}: {st}: {e}");
            }
            0
        }
        Err(f) => {
            let _ = writeln!(err, "[RED] {f}");
            1
        }
    }
}
