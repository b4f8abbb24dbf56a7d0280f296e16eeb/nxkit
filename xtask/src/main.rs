//! Workspace checks that rustc and clippy cannot do.
//!
//! - `layers`: every crate declares `[package.metadata.nxb] layer` and `in_v`, dependencies point to a lower layer or
//!   are listed sideways edges (docs/workspace-layout.md 2 and 2.1), dev crates are never normal dependencies, a crate in
//!   the chain version V depends only on crates in V.
//!   The front-end crates (`msl-*`, `air-*`, `airc`, `mslcheck`) are layer `F` (ADR W20): they depend only on each other and
//!   on no `nxb-*` crate, and only the entry points `air-front` and `airc` may be used from outside, by `L2` or `L3` crates.
//!   The title crates are layer `T`: a family or title crate depends only on `nxtitle` and `L0`, and only the registry
//!   `nxtitles` depends on a `T` crate.
//! - `limits`: source files at most 600 lines, stage modules of `nxb-stages` at most 400.
//!
//! Usage: `cargo run -p xtask -- [layers] [limits]` (both when no argument is given). Std only, the TOML reading is a
//! deliberately small line parser for the fields above.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Allowed edges between crates of the same layer.
const SIDEWAYS: &[(&str, &str)] = &[
    ("nxtitles", "nxtitle"),
    ("nxb-error", "nxb-model"),
    ("nxb-model", "nxb-sys"),
    ("nxb-air", "nxb-shader"),
    ("nxb-shader", "nxb-pyjson"),
    ("nxb-xlate", "nxb-pyjson"),
    ("nxb-hos", "nxb-romfs"),
    ("nxb-hos", "nxb-crypto"),
    ("nxb-hos", "nxb-codec"),
    ("nxb-romfs", "nxb-crypto"),
    ("nxb-rewrite", "nxb-a64"),
    ("nxb-rewrite", "nxb-codec"),
    ("nxb-split", "nxb-codec"),
    ("nxb-split", "nxb-romfs"),
    ("nxb-bundle", "nxb-plist"),
    ("nxb-codesign", "nxb-plist"),
    ("nxb-codesign", "nxb-crypto"),
    ("nxb-zip", "nxb-codec"),
    ("nxb-pso", "nxb-pyjson"),
    ("nxb-cli", "nxb-stages"),
    // ADR W19: stage crates beside the wiring (L3, they do I/O), the orchestrator and the CLI call them
    ("nxb-cli", "nxb-extract"),
    ("nxb-cli", "nxb-pack"),
    ("nxb-cli", "nxb-archive"),
    ("nxb-cli", "nxb-build"),
    ("nxb-build", "nxb-extract"),
    ("nxb-build", "nxb-pack"),
    ("nxb-build", "nxb-archive"),
    ("nxb-build", "nxb-stages"),
    // The nxkit host: the services and the kernel use the guest ABI of `nxkit-abi`, and `nxkit-rt` puts the NVN C
    // functions over the front end of `nxkit-nvn`.
    ("nxkit-hos", "nxkit-abi"),
    ("nxkit-ipc", "nxkit-abi"),
    ("nxkit-svc-audio", "nxkit-abi"),
    ("nxkit-svc-fs", "nxkit-abi"),
    ("nxkit-svc-input", "nxkit-abi"),
    ("nxkit-svc-offline", "nxkit-abi"),
    ("nxkit-svc-pacing", "nxkit-abi"),
    ("nxkit-svc-system", "nxkit-abi"),
    ("nxkit-svc-video", "nxkit-abi"),
    ("nxkit-rt", "nxkit-nvn"),
];

/// Edges from a crate in V to a crate outside V that are output-neutral by contract (ADR W17: hardware SHA-256 gives
/// the same digests as the scalar path, tested on the same vectors).
const V_EXEMPT: &[(&str, &str)] = &[("nxb-model", "nxb-sys")];

/// The front-end crates that `nxb-*` crates may depend on (ADR W20): the in-process front end and the command line.
const FRONT_ENTRIES: &[&str] = &["air-front", "airc"];

/// The folders that hold crates, one crate per subfolder.
const CRATE_DIRS: &[&str] = &["crates", "titles", "titles/families", "titles/games"];

/// The title API, the one crate a `T` crate may use besides `L0`.
const TITLE_API: &str = "nxtitle";

/// The registry, the one crate that may use a `T` crate.
const TITLE_REGISTRY: &str = "nxtitles";

const FILE_LIMIT: usize = 600;
const STAGE_MODULE_LIMIT: usize = 400;

#[derive(Debug, Default)]
struct Krate {
    name: String,
    layer: String,
    in_v: Option<bool>,
    deps: Vec<String>,
    dir: PathBuf,
}

fn parse_manifest(path: &Path) -> Result<Krate, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut k = Krate {
        dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        ..Krate::default()
    };
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim().trim_matches('"'));
        match section.as_str() {
            "package" if key == "name" => k.name = value.to_string(),
            "package.metadata.nxb" if key == "layer" => k.layer = value.to_string(),
            "package.metadata.nxb" if key == "in_v" => k.in_v = Some(value == "true"),
            "dependencies" => {
                let dep = key.split('.').next().unwrap_or(key).trim();
                k.deps.push(dep.to_string());
            }
            _ => {}
        }
    }
    Ok(k)
}

fn rank(layer: &str) -> Option<u8> {
    match layer {
        "L0" => Some(0),
        "L1" => Some(1),
        "L2" => Some(2),
        "L3" => Some(3),
        "T" => Some(1),
        "F" => Some(8),
        "dev" => Some(9),
        _ => None,
    }
}

fn crates(root: &Path) -> Result<Vec<Krate>, String> {
    let mut manifests: Vec<PathBuf> = Vec::new();
    for sub in CRATE_DIRS {
        let dir = root.join(sub);
        let rd = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        manifests.extend(
            rd.flatten()
                .map(|e| e.path().join("Cargo.toml"))
                .filter(|p| p.exists()),
        );
    }
    manifests.push(root.join("xtask/Cargo.toml"));
    manifests.sort();
    manifests.iter().map(|p| parse_manifest(p)).collect()
}

fn check_layers(all: &[Krate]) -> Vec<String> {
    let mut errs = Vec::new();
    let by_name: BTreeMap<&str, &Krate> = all.iter().map(|k| (k.name.as_str(), k)).collect();
    for k in all {
        let Some(r) = rank(&k.layer) else {
            errs.push(format!(
                "{}: missing or unknown [package.metadata.nxb] layer",
                k.name
            ));
            continue;
        };
        if k.in_v.is_none() {
            errs.push(format!("{}: missing [package.metadata.nxb] in_v", k.name));
        }
        for d in k
            .deps
            .iter()
            .filter(|d| d.starts_with("nxb-") || by_name.contains_key(d.as_str()))
        {
            let Some(dk) = by_name.get(d.as_str()) else {
                errs.push(format!("{} -> {d}: unknown workspace crate", k.name));
                continue;
            };
            if k.layer == "T" || dk.layer == "T" {
                // A family or title uses only the title API and L0. Only the registry uses a family or title.
                let ok = if dk.layer == "T" {
                    k.name == TITLE_REGISTRY
                } else {
                    d == TITLE_API || dk.layer == "L0"
                };
                if !ok {
                    errs.push(format!(
                        "{} ({}) -> {d} ({}): title layer rule",
                        k.name, k.layer, dk.layer
                    ));
                }
                continue;
            }
            if k.layer == "F" || dk.layer == "F" {
                // ADR W20: F depends only on F. Outside crates may use only the entry points, from L2 or L3.
                let ok = if k.layer == "F" {
                    dk.layer == "F"
                } else {
                    FRONT_ENTRIES.contains(&d.as_str()) && (k.layer == "L2" || k.layer == "L3")
                };
                if !ok {
                    errs.push(format!(
                        "{} ({}) -> {d} ({}): front-end layer rule (ADR W20)",
                        k.name, k.layer, dk.layer
                    ));
                }
                continue;
            }
            let Some(dr) = rank(&dk.layer) else { continue };
            let sideways = SIDEWAYS.contains(&(k.name.as_str(), d.as_str()));
            if dk.layer == "dev" {
                errs.push(format!(
                    "{} -> {d}: dev crates are dev-dependencies only",
                    k.name
                ));
            } else if dr > r || (dr == r && !sideways) {
                errs.push(format!(
                    "{} ({}) -> {d} ({}): not downward and not a listed sideways edge",
                    k.name, k.layer, dk.layer
                ));
            }
            if k.in_v == Some(true)
                && dk.in_v != Some(true)
                && !V_EXEMPT.contains(&(k.name.as_str(), d.as_str()))
            {
                errs.push(format!(
                    "{} is in V but depends on {d}, which is not",
                    k.name
                ));
            }
        }
    }
    errs
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn check_limits(all: &[Krate]) -> Vec<String> {
    let mut errs = Vec::new();
    for k in all {
        if k.layer == "F" {
            continue;
        }
        let mut files = Vec::new();
        rs_files(&k.dir.join("src"), &mut files);
        for f in files {
            let lines = fs::read_to_string(&f)
                .map(|s| s.lines().count())
                .unwrap_or(0);
            let stage_module =
                k.name == "nxb-stages" && f.file_name().is_some_and(|n| n != "lib.rs");
            let limit = if stage_module {
                STAGE_MODULE_LIMIT
            } else {
                FILE_LIMIT
            };
            if lines > limit {
                errs.push(format!("{}: {lines} lines (limit {limit})", f.display()));
            }
        }
    }
    errs
}

fn main() -> ExitCode {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        args = vec!["layers".into(), "limits".into()];
    }
    let all = match crates(&root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xtask: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = false;
    for a in &args {
        let errs = match a.as_str() {
            "layers" => check_layers(&all),
            "limits" => check_limits(&all),
            other => vec![format!("unknown check {other:?} (layers, limits)")],
        };
        if errs.is_empty() {
            println!("xtask {a}: ok ({} crates)", all.len());
        }
        for e in &errs {
            eprintln!("xtask {a}: {e}");
        }
        failed |= !errs.is_empty();
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(name: &str, layer: &str, in_v: bool, deps: &[&str]) -> Krate {
        Krate {
            name: name.into(),
            layer: layer.into(),
            in_v: Some(in_v),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            dir: PathBuf::new(),
        }
    }

    #[test]
    fn layer_rules() {
        let ok = [
            k("nxb-model", "L0", true, &[]),
            k("nxb-error", "L0", true, &["nxb-model"]),
            k("nxb-store", "L2", false, &["nxb-model", "nxb-error"]),
        ];
        assert!(check_layers(&ok).is_empty());
        let up = [
            k("nxb-model", "L0", true, &["nxb-store"]),
            k("nxb-store", "L2", false, &[]),
        ];
        assert_eq!(check_layers(&up).len(), 2, "up edge and V leak");
        let side = [
            k("nxb-tool", "L2", false, &["nxb-store"]),
            k("nxb-store", "L2", false, &[]),
        ];
        assert_eq!(check_layers(&side).len(), 1);
        let dev = [
            k("nxb-obs", "L2", false, &["nxb-testkit"]),
            k("nxb-testkit", "dev", false, &[]),
        ];
        assert_eq!(check_layers(&dev).len(), 1);
        let front = [
            k("air-front", "F", false, &["air-emit"]),
            k("air-emit", "F", false, &[]),
            k("nxb-tool", "L2", false, &["air-front"]),
        ];
        assert!(check_layers(&front).is_empty());
        let bad_front = [
            k("air-emit", "F", false, &["nxb-model"]),
            k("nxb-model", "L0", true, &[]),
            k("nxb-tool", "L2", false, &["air-emit"]),
        ];
        assert_eq!(check_layers(&bad_front).len(), 2);
        let titles = [
            k("nxb-model", "L0", true, &[]),
            k("nxtitle", "L1", false, &["nxb-model"]),
            k(
                "nxtitles",
                "L1",
                false,
                &["nxtitle", "nxfamily-a", "nxgame-b"],
            ),
            k("nxfamily-a", "T", false, &["nxtitle"]),
            k("nxgame-b", "T", false, &["nxtitle", "nxb-model"]),
        ];
        assert!(check_layers(&titles).is_empty());
        let bad_titles = [
            k("nxtitle", "L1", false, &[]),
            k("nxb-store", "L2", false, &[]),
            k("nxfamily-a", "T", false, &["nxtitle"]),
            k("nxgame-b", "T", false, &["nxfamily-a", "nxb-store"]),
            k("nxb-build", "L3", false, &["nxgame-b"]),
        ];
        assert_eq!(check_layers(&bad_titles).len(), 3);
    }

    #[test]
    fn this_workspace_passes() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let all = crates(&root).unwrap();
        assert!(all.len() > 20);
        assert_eq!(check_layers(&all), Vec::<String>::new());
        assert_eq!(check_limits(&all), Vec::<String>::new());
    }
}
