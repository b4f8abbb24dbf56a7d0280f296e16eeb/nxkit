//! The command line of `nxb build`: exit code 2 for bad arguments.

use std::path::PathBuf;

/// The command line of `nxb build`.
#[derive(Clone, Debug, Default)]
pub struct Args {
    /// The folder holding one game's packages, or its `.nup` file.
    pub nsp_dir: PathBuf,
    /// `-k/--keys`.
    pub keys: Option<PathBuf>,
    /// `--title-id` / `--app`.
    pub title_id: Option<String>,
    /// `-o/--out`.
    pub out: Option<PathBuf>,
    /// `--app-name`.
    pub app_name: Option<String>,
    /// `--bundle-id`.
    pub bundle_id: Option<String>,
    /// `--platform` (`mac` or `mac-app`: the same app).
    pub platform: String,
    /// `--work-dir`.
    pub work_dir: Option<PathBuf>,
    /// `--gpu` (`none` or architectures).
    pub gpu: Option<String>,
    /// `--pipeline-list FILE`: the list of recorded pipelines (`pipelines.seen` or `seen-<scheme>.list` of the host)
    /// whose states the pipeline archive compiles. Without it the build takes the lists of the app it replaces.
    pub pipeline_list: Option<PathBuf>,
    /// `--progress`.
    pub progress: String,
    /// `--auto-analyze`: use input metadata, with no title-specific patches.
    pub auto_analyze: bool,
    /// `--dry-run`: print the plan and write nothing.
    pub dry_run: bool,
    /// Worker threads, `--threads N`. The default is this Mac's P-core equivalents, `round(P + 0.4 × E)`
    /// (10 on an M2 Max with 8 P and 4 E cores).
    pub threads: usize,
    /// `--load-limit <percent>`: the pipeline archive starts more compiler runs only while CPU and memory use stay
    /// below this share of the machine (default 90).
    pub load_limit: u8,
    /// `--own-front on|off|<verdicts>`: the own front end (default on: the raw tier, and own-identical shaders when verdicts exist.
    /// `NXB_OWN_FRONT=0` turns it off).
    pub own_front: String,
    /// `--shader-manifest <file>`: the keys the title used, one per line. Their shaders are built first.
    pub manifest: Option<PathBuf>,
    /// `--allow-damaged`: files failing the NCA hash check are written as read and the build goes on.
    pub allow_damaged: bool,
    /// `--partial`: build only the manifest's shaders and flag the output as partial (needs `--shader-manifest`).
    pub partial: bool,
    /// `--nup <file>`: a NUP container (a `.nup` file as the input, or the only one in the folder, is found as well).
    pub nup: Option<PathBuf>,
    /// `--aoc <package>` (repeatable): DLC packages outside the input folder.
    pub aoc: Vec<PathBuf>,
    /// `--update <file-or-directory>` (repeatable), for `--auto-analyze`.
    pub update_inputs: Vec<PathBuf>,
    /// `--base-package <file>`: original encrypted backing for generic NUP updates.
    pub base_package: Option<PathBuf>,
    /// `--resource-patch <option>=<value|stock>` (repeatable): a title's resource patch with its value, `stock` for none
    /// (`resolution-scale=0.75`).
    pub resource_patches: Vec<(String, String)>,
}

const VALUE: &[&str] = &[
    "-k",
    "--keys",
    "--title-id",
    "--app",
    "-o",
    "--out",
    "--app-name",
    "--bundle-id",
    "--platform",
    "--work-dir",
    "--gpu",
    "--pipeline-list",
    "--progress",
    "--threads",
    "--load-limit",
    "--own-front",
    "--shader-manifest",
    "--nup",
    "--aoc",
    "--update",
    "--base-package",
    "--resource-patch",
];
/// Switches without a value.
fn switch(a: &mut Args, k: &str) -> bool {
    match k {
        "--allow-damaged" => a.allow_damaged = true,
        "--partial" => a.partial = true,
        "--auto-analyze" => a.auto_analyze = true,
        "--dry-run" => a.dry_run = true,
        _ => return false,
    }
    true
}
/// `none`, or architectures such as `g14s`, `applegpu_g14s`, comma-separated.
fn gpu_list(g: &str) -> bool {
    g == "none"
        || g.split(',').all(|x| {
            let r = x.strip_prefix("applegpu_").unwrap_or(x);
            r.starts_with('g')
                && r.len() > 1
                && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The checks of option combinations (exit 2).
fn validate(a: &Args) -> Result<(), (i32, String)> {
    if a.base_package.is_some() && (!a.auto_analyze || a.update_inputs.is_empty()) {
        return Err((
            2,
            "--base-package requires --auto-analyze and an explicit --update package".into(),
        ));
    }
    if !a.auto_analyze && !a.update_inputs.is_empty() {
        return Err((2, "--update requires --auto-analyze".into()));
    }
    if let Some(g) = a.gpu.as_deref().map(str::trim).filter(|g| !g.is_empty())
        && !gpu_list(g)
    {
        return Err((
            2,
            format!("--gpu takes GPU architectures such as g14s or g18p,g17p, or none: {g}"),
        ));
    }
    Ok(())
}

/// The default of `--threads`: the P-core equivalents of this Mac.
pub(crate) fn default_threads() -> usize {
    nxb_sched::Budget::detect(nxb_sched::DEFAULT_E_FACTOR, 0).tokens as usize
}

/// Parses the command line.`Err((code, message))`.
pub fn parse(argv: &[String]) -> Result<Args, (i32, String)> {
    let mut a = Args {
        platform: "mac".into(),
        progress: "auto".into(),
        threads: default_threads(),
        load_limit: 90,
        ..Args::default()
    };
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        let (k, inline) = match s.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (s, None),
        };
        if switch(&mut a, k) {
            i += 1;
            continue;
        }
        if VALUE.contains(&k) {
            let v = match inline {
                Some(v) => v,
                None => {
                    i += 1;
                    argv.get(i)
                        .cloned()
                        .ok_or((2, format!("argument {k}: expected one argument")))?
                }
            };
            match k {
                "-k" | "--keys" => a.keys = Some(v.into()),
                // `--app` names the application like `nx_extract --app`: the same game selection as `--title-id`
                "--title-id" | "--app" => a.title_id = Some(v),
                "--nup" => a.nup = Some(v.into()),
                "--aoc" => a.aoc.push(v.into()),
                "--update" => a.update_inputs.push(v.into()),
                "--base-package" => a.base_package = Some(v.into()),
                "--resource-patch" => {
                    let (name, value) = v
                        .split_once('=')
                        .filter(|(n, x)| !n.is_empty() && !x.is_empty())
                        .ok_or((
                            2,
                            format!("--resource-patch: {v} (expected <option>=<value>)"),
                        ))?;
                    a.resource_patches.push((name.into(), value.into()));
                }
                "-o" | "--out" => a.out = Some(v.into()),
                "--app-name" => a.app_name = Some(v),
                "--bundle-id" => a.bundle_id = Some(v),
                "--platform" => a.platform = v,
                "--work-dir" => a.work_dir = Some(v.into()),
                "--gpu" => a.gpu = Some(v),
                "--pipeline-list" => a.pipeline_list = Some(v.into()),
                "--progress" => a.progress = v,
                "--own-front" => a.own_front = v,
                "--load-limit" => {
                    a.load_limit = v
                        .trim_end_matches('%')
                        .parse()
                        .ok()
                        .filter(|p| (10..=100).contains(p))
                        .ok_or((2, format!("--load-limit: {v} (a percent from 10 to 100)")))?;
                }
                "--shader-manifest" => a.manifest = Some(v.into()),
                _ => a.threads = v.parse().map_err(|_| (2, format!("--threads: {v}")))?,
            }
        } else if s.starts_with('-') {
            return Err((2, format!("unrecognized arguments: {s}")));
        } else if a.nsp_dir.as_os_str().is_empty() {
            a.nsp_dir = s.into();
        } else {
            return Err((2, format!("unrecognized arguments: {s}")));
        }
        i += 1;
    }
    if !["mac", "mac-app"].contains(&a.platform.as_str()) {
        return Err((
            2,
            format!(
                "argument --platform: invalid choice: '{}' (choose from mac, mac-app)",
                a.platform
            ),
        ));
    }
    validate(&a)?;
    if a.partial && a.manifest.is_none() {
        return Err((2, "--partial needs --shader-manifest <file>".into()));
    }
    if a.nsp_dir.as_os_str().is_empty() {
        match &a.nup {
            Some(n) => a.nsp_dir = n.clone(),
            None => {
                return Err((
                    2,
                    "the input is required: a folder of packages or a .nup file".into(),
                ));
            }
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn backing_requires_generic_mode_and_explicit_update() {
        for args in [
            vec!["game.nup", "--base-package", "base.nsp"],
            vec!["game.nup", "--auto-analyze", "--base-package", "base.nsp"],
            vec![
                "game.nup",
                "--base-package",
                "base.nsp",
                "--update",
                "patch.nsp",
            ],
        ] {
            assert!(parse(&v(&args)).is_err());
        }
        let a = parse(&v(&[
            "game.nup",
            "--auto-analyze",
            "--base-package=base.nsp",
            "--update",
            "patch.nsp",
        ]))
        .unwrap();
        assert_eq!(a.base_package, Some(PathBuf::from("base.nsp")));
    }

    #[test]
    fn separate_updates_require_generic_mode() {
        assert!(parse(&v(&["dir", "--update", "patch.nsz"])).is_err());
        let a = parse(&v(&[
            "dir",
            "--auto-analyze",
            "--update",
            "patch.nsz",
            "--update=updates",
        ]))
        .unwrap();
        assert_eq!(
            a.update_inputs,
            vec![PathBuf::from("patch.nsz"), PathBuf::from("updates")]
        );
        assert!(parse(&v(&["dir", "--auto-analyze", "--update"])).is_err());
    }

    #[test]
    fn the_command_line() {
        let a = parse(&v(&[
            "dir",
            "--platform",
            "mac-app",
            "-k",
            "k.keys",
            "--gpu=none",
            "--progress",
            "json",
        ]));
        assert!(a.as_ref().is_ok_and(|a| a.keys.is_some()
            && a.gpu.as_deref() == Some("none")
            && a.progress == "json"));
        let code = |a: &[&str]| parse(&v(a)).err().map(|e| e.0);
        assert_eq!(parse(&v(&["dir"])).map(|a| a.load_limit), Ok(90));
        assert!(parse(&v(&["dir"])).is_ok_and(|a| a.platform == "mac"));
        let threads = parse(&v(&["dir"])).map(|a| a.threads).unwrap();
        assert_eq!(threads, default_threads());
        assert!(threads >= 1);
        assert_eq!(
            parse(&v(&["dir", "--threads", "4"])).map(|a| a.threads),
            Ok(4)
        );
        assert_eq!(
            parse(&v(&["dir", "--load-limit", "75%"])).map(|a| a.load_limit),
            Ok(75)
        );
        assert_eq!(code(&["dir", "--load-limit", "5"]), Some(2));
        assert_eq!(code(&["dir", "--load-limit=101"]), Some(2));
        assert_eq!(code(&["dir", "--gpu", "m2"]), Some(2));
        assert!(parse(&v(&["dir", "--gpu", "g14s,applegpu_g18p"])).is_ok());
        assert_eq!(code(&["dir", "--platform", "tv"]), Some(2));
        assert_eq!(code(&["dir", "--platform", "ios"]), Some(2));
        // the options of the engine path are unknown now
        for gone in ["--engine", "--shaders", "--program", "--dump", "--romfs"] {
            assert_eq!(code(&["dir", gone, "x"]), Some(2), "{gone}");
        }
        assert_eq!(code(&["dir", "--prepare-xcode"]), Some(2));
        let a = parse(&v(&["dir", "--shader-manifest", "m.txt", "--partial"])).unwrap();
        assert!(a.partial && a.manifest.as_deref() == Some(std::path::Path::new("m.txt")));
        assert_eq!(code(&["dir", "--partial"]), Some(2));
        assert!(
            parse(&v(&["game.nsp", "--auto-analyze", "-k", "keys"])).is_ok_and(|a| a.auto_analyze)
        );
        assert!(!parse(&v(&["dir"])).unwrap().auto_analyze);
        assert_eq!(code(&[]), Some(2));
    }
}
