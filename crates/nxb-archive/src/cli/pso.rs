//! `nxb-pso --msl <b-<chain>> --out <dir> --work <dir> --store <dir> --pairs <tsv> [--pairs <tsv>] [--depth gl|zero]
//! [--gpu g18p[,g17p]|none] [--platform ios|macos] [--os-min 27.0] [--script-only] [--target 1000] [--tokens N]
//! [--shader-map <tsv>] [--manifest <tsv>] [--pipeline-list <list>] [--no-compute]`
//!
//! `pso_offline.py` in Rust: the plan (pairs to bases), the script, `prebuilt.tsv`, `rest_pairs.tsv`, `archive.tsv`, and
//! one archive per architecture through [`crate::build`] (content-cut batches, store, merge cache).
//! Every `--shader-map` and `--manifest` adds compute shaders: the `C` rows of the manifests first, then the compute
//! rows of the shader maps. Each gets a compute pipeline in the archive. `--no-compute` builds the bases alone.
//! Every `--pipeline-list` (a `pipelines.seen` or `seen-<scheme>.list` of the host) adds the states that the title recorded
//! and the plain base cannot serve (vertex layout, samples, alpha to coverage, ICB support) as bases ([`crate::seen`]).
//! Each recorded render pipeline of them is added again fully specialized ([`crate::exact`]), so the host's lookup of its exact descriptor hits.
//! `--gpu none` writes the stamp and tables but no archive (the Mac's "no offline archive" mode).

use std::path::PathBuf;

/// `nxb pso` / `nxb-pso`: returns the exit code (0 done, 1 failed).
pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            println!("[RED] pso_offline: {e}");
            1
        }
    }
}

fn run(a: &[String]) -> Result<(), String> {
    let mut pairs = Vec::new();
    let (mut maps, mut manifests, mut lists) = (Vec::new(), Vec::new(), Vec::new());
    let mut o = std::collections::BTreeMap::new();
    let mut flags = std::collections::BTreeSet::new();
    let mut i = 0;
    while i < a.len() {
        let k = a[i].trim_start_matches('-').to_string();
        if k == "script-only" || k == "no-compute" {
            flags.insert(k);
            i += 1;
            continue;
        }
        let v = a
            .get(i + 1)
            .cloned()
            .ok_or_else(|| format!("{k} needs a value"))?;
        if k == "pairs" {
            pairs.push(PathBuf::from(v));
        } else if k == "shader-map" {
            maps.push(PathBuf::from(v));
        } else if k == "pipeline-list" {
            lists.push(PathBuf::from(v));
        } else if k == "manifest" {
            manifests.push(PathBuf::from(v));
        } else {
            o.insert(k, v);
        }
        i += 2;
    }
    let get = |k: &str| {
        o.get(k)
            .cloned()
            .ok_or_else(|| format!("--{k} is required"))
    };
    let lib_dir = PathBuf::from(get("msl")?);
    let (out, work) = (PathBuf::from(get("out")?), PathBuf::from(get("work")?));
    let chain = lib_dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("b-"))
        .ok_or_else(|| format!("{} is not a b-<chain version> folder", lib_dir.display()))?
        .to_string();
    let gpu = o.get("gpu").cloned().unwrap_or_else(|| "g18p".into());
    let mut archs: Vec<String> = if gpu == "none" {
        Vec::new()
    } else {
        let s: std::collections::BTreeSet<String> = gpu
            .split(',')
            .filter(|x| !x.trim().is_empty())
            .map(crate::plan::arch_name)
            .collect();
        s.into_iter().collect()
    };
    let zero = o.get("depth").map(String::as_str) == Some("zero");
    let text = crate::plan::pair_text(&pairs);
    if text.is_empty() {
        return Err("no pair list".into());
    }
    let t0 = std::time::Instant::now();
    let ps = crate::plan::read_pairs(&text);
    let p = crate::plan::plan(&lib_dir, &chain, &ps, zero);
    let shaders = if flags.contains("no-compute") {
        Vec::new()
    } else {
        crate::compute::read_computes_with(&maps, &manifests, &lists)
    };
    let (computes, cleft) = crate::compute::plan_compute(&lib_dir, &chain, &shaders);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let js = work.join("bases.mtl4-json");
    let (states, refused) =
        crate::seen::usable(&lib_dir, &chain, &crate::seen::read_lists(&lists), zero);
    let all_exacts = crate::exact::read_lists(&lists);
    let (exacts, exact_refused) = crate::exact::usable(&lib_dir, &chain, &all_exacts, zero);
    let script = crate::plan::script_text_full(&lib_dir, &p.bases, &states, &exacts, &computes);
    if std::fs::read(&js).ok().as_deref() != Some(script.as_bytes()) {
        std::fs::write(&js, &script).map_err(|e| e.to_string())?;
    }
    let st = crate::plan::stamp(&chain, zero, &text);
    let l = p.left;
    println!(
        "pso_offline: {} pairs: {} bases, {} left to the device (function constants), left out as the host leaves them: reflection {}, mesh {}, stage inputs {}, library {}, layer {}, primitive ID {}, entry {} ({:.1} s)",
        ps.len(),
        p.bases.len(),
        p.rest.len(),
        l[0],
        l[1],
        l[2],
        l[3],
        l[4],
        l[5],
        l[6],
        t0.elapsed().as_secs_f64()
    );
    if !lists.is_empty() {
        println!(
            "pso_offline: {} recorded states added as bases, {} left out as invalid",
            states.len(),
            refused.len()
        );
        for r in refused.iter().take(5) {
            println!("pso_offline:   {}: {}", r.what, r.reason);
        }
        println!(
            "pso_offline: {} of {} recorded render pipelines added fully specialized, {} left out as invalid",
            exacts.len(),
            all_exacts.len(),
            exact_refused.len()
        );
        for r in exact_refused.iter().take(5) {
            println!("pso_offline:   {}: {}", r.what, r.reason);
        }
    }
    if !shaders.is_empty() {
        println!(
            "pso_offline: {}",
            crate::compute::summary(shaders.len(), computes.len(), cleft)
        );
    }
    let write = |name: &str, t: &str| std::fs::write(out.join(name), t).map_err(|e| e.to_string());
    if flags.contains("script-only") {
        return write("prebuilt.tsv", &st);
    }
    if !ps.is_empty() && p.bases.is_empty() && p.rest.is_empty() {
        return Err("no base to build".into());
    }
    if p.bases.is_empty() && computes.is_empty() && states.is_empty() && exacts.is_empty() {
        archs.clear();
    }
    let platform = o.get("platform").cloned().unwrap_or_else(|| "ios".into());
    let osmin = o
        .get("os-min")
        .cloned()
        .ok_or("--os-min is required (pso_offline's default reads sw_vers / the SDK)")?;
    let mut toolchain = String::from("not used (function constants only)");
    for arch in &archs {
        let mut c = crate::Config::for_tools(
            &platform,
            arch,
            &osmin,
            &js,
            &out.join(format!("{arch}.mtl4a")),
            &work,
        )?;
        c.store = PathBuf::from(get("store")?);
        if let Some(t) = o.get("target").and_then(|t| t.parse().ok()) {
            c.target = t;
        }
        if let Some(t) = o.get("tokens").and_then(|t| t.parse().ok()) {
            c.tokens = t;
        }
        toolchain = c.toolchain.clone();
        println!(
            "pso_offline: {arch}: {} bases, {} compute pipelines (minutes) ...",
            p.bases.len(),
            computes.len()
        );
        let r = crate::build(&c)?;
        println!(
            "pso_offline: {arch}: {} MB, {} batches ({} from the store), merged {}",
            r.size >> 20,
            r.batches.len(),
            r.batches.iter().filter(|b| b.reused).count(),
            if r.merged_from_store {
                "from the store"
            } else {
                "now"
            }
        );
        for d in &r.dropped {
            println!(
                "pso_offline: {arch}: left out, the compiler refused it: {}: {}",
                d.what, d.reason
            );
        }
        if !r.dropped.is_empty() {
            println!(
                "pso_offline: {arch}: {} recorded states left out",
                r.dropped.len()
            );
        }
    }
    if let Ok(rd) = std::fs::read_dir(&out) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".mtl4a")
                && !archs.iter().any(|a| a == stem)
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    write("prebuilt.tsv", &st)?;
    match crate::plan::rest_tsv(&p.rest) {
        Some(t) => write("rest_pairs.tsv", &t)?,
        None => {
            let _ = std::fs::remove_file(out.join("rest_pairs.tsv"));
        }
    }
    write(
        "archive.tsv",
        &crate::plan::archive_tsv(
            &archs,
            &platform,
            &osmin,
            &toolchain,
            p.bases.len(),
            p.rest.len(),
            computes.len(),
        ),
    )
}
