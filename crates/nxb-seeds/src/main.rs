//! `nxb-seeds import --engine <checkout> [--ref <ref>] [--titles <folder>] [--only <title id>]`
//!
//! Reads `dm/games/<title id>/pso_manifest.tsv`, `shader_seen.tsv` and the `depth_mode` of `game.tsv` of every title of the old engine at `<ref>`
//! (default `upstream/main`) through `git show`, so the checkout is only read. For each title that has a module in
//! `<folder>` (default `titles/games`) it writes `pipelines.seeds` there. The other titles are listed at the end with
//! their number of render pipeline rows. A rerun on the same commit writes the same bytes. A shader seed whose shader a
//! pipeline seed names is left out: the build translates it anyway.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use nxb_seeds::import::Imported;
use nxkit_pipestate::seeds::{DepthMode, FILE, Seed};

/// The largest file `tools/no-game-data.sh` lets into the tree.
const MAX_BYTES: usize = 1 << 20;

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn is_title(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

struct Args {
    engine: PathBuf,
    reference: String,
    titles: PathBuf,
    only: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut a = std::env::args().skip(1);
    if a.next().as_deref() != Some("import") {
        return Err("usage: nxb-seeds import --engine <checkout> [--ref <ref>] [--titles <folder>] [--only <title id>]".into());
    }
    let mut out = Args {
        engine: PathBuf::new(),
        reference: "upstream/main".into(),
        titles: PathBuf::from("titles/games"),
        only: None,
    };
    while let Some(k) = a.next() {
        let v = a.next().ok_or_else(|| format!("{k} needs a value"))?;
        match k.as_str() {
            "--engine" => out.engine = v.into(),
            "--ref" => out.reference = v,
            "--titles" => out.titles = v.into(),
            "--only" => out.only = Some(v),
            _ => return Err(format!("unknown option {k}")),
        }
    }
    if out.engine.as_os_str().is_empty() {
        return Err("--engine names the checkout of the old engine".into());
    }
    Ok(out)
}

fn run(a: &Args) -> Result<(), String> {
    let r = &a.reference;
    let commit = git(&a.engine, &["rev-parse", "--short=12", r])
        .ok_or_else(|| format!("{}: no commit {r}", a.engine.display()))?;
    let commit = commit.trim();
    let listing = git(&a.engine, &["ls-tree", "--name-only", r, "dm/games/"])
        .ok_or_else(|| format!("{r}: no dm/games"))?;
    let ids: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.strip_prefix("dm/games/"))
        .filter(|id| is_title(id))
        .filter(|id| a.only.as_deref().is_none_or(|o| o == *id))
        .collect();
    let mut missing = Vec::new();
    let (mut written, mut unchanged) = (0, 0);
    for id in ids {
        let show = |f: &str| git(&a.engine, &["show", &format!("{r}:dm/games/{id}/{f}")]);
        let mut imp = Imported::default();
        if let Some(t) = show("pso_manifest.tsv") {
            imp.manifest(&t);
        }
        if let Some(t) = show("shader_seen.tsv") {
            imp.shader_seen(&t);
        }
        let count = |f: fn(&Seed) -> bool| imp.seeds.seeds().iter().filter(|s| f(s)).count();
        let renders = count(|s| matches!(s, Seed::Render(_)));
        let pipelines = renders + count(|s| matches!(s, Seed::Mesh(_) | Seed::Compute(_)));
        let module = a.titles.join(id);
        if !module.is_dir() {
            let name = show("README.md")
                .and_then(|t| {
                    t.lines()
                        .next()
                        .map(|l| l.trim_start_matches("# ").to_string())
                })
                .unwrap_or_default();
            missing.push((id.to_string(), renders, name));
            continue;
        }
        if pipelines == 0 {
            continue;
        }
        let l = imp.left;
        let comments = vec![
            format!(
                "Imported by nxb-seeds from the old engine at {commit}: dm/games/{id}/pso_manifest.tsv, shader_seen.tsv and game.tsv."
            ),
            format!(
                "Left out: {} rows of merged passes, {} with a target format the host has no name for, {} that do not parse.",
                l.merged, l.format, l.damaged
            ),
        ];
        let mut seeds = imp.without_named_shaders();
        seeds.depth = show("game.tsv").and_then(|t| {
            t.lines()
                .find_map(|l| match l.split('\t').collect::<Vec<_>>()[..] {
                    ["depth_mode", "zero", ..] => Some(DepthMode::Zero),
                    ["depth_mode", "gl", ..] => Some(DepthMode::Gl),
                    _ => None,
                })
        });
        let text = seeds.to_text(&comments);
        if text.len() > MAX_BYTES {
            return Err(format!(
                "{id}: {} bytes, more than the tree allows",
                text.len()
            ));
        }
        let path = module.join(FILE);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            unchanged += 1;
            continue;
        }
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        written += 1;
        println!(
            "{id}: {} seeds ({renders} render, {pipelines} pipelines), {} rows left out, {} bytes",
            seeds.len(),
            l.total(),
            text.len()
        );
    }
    println!("{written} files written, {unchanged} unchanged");
    if !missing.is_empty() {
        println!(
            "\ntitles of the old engine without a module here ({}):",
            missing.len()
        );
        for (id, renders, name) in missing {
            println!("{id}\t{renders} render rows\t{name}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match args().and_then(|a| run(&a)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nxb-seeds: {e}");
            ExitCode::FAILURE
        }
    }
}
