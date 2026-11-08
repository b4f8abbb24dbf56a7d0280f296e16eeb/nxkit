//! The command line of `nxb build`: the NSP script's names and exit codes (2 bad arguments, 4 an option or a case that
//! is not implemented yet).

use std::path::PathBuf;

/// The command line (the NSP script's names, extras: `--dry-run`, `--engine`, `--shaders`, `--companion-app`, `--threads`,
/// `--load-limit`).
#[derive(Clone, Debug, Default)]
pub struct Args {
    /// The folder holding one game's NSPs.
    pub nsp_dir: PathBuf,
    /// `-k/--keys`.
    pub keys: Option<PathBuf>,
    /// `--title-id`.
    pub title_id: Option<String>,
    /// `-p/--package`.
    pub package: Option<String>,
    /// `-o/--out`.
    pub out: Option<PathBuf>,
    /// `--app-name`.
    pub app_name: Option<String>,
    /// `--bundle-id`.
    pub bundle_id: Option<String>,
    /// `--platform` (`mac-app` or `ios`).
    pub platform: String,
    /// `--mac-sign` (only ad hoc `-`).
    pub mac_sign: String,
    /// `-l/--language`.
    pub language: Option<String>,
    /// `--work-dir`.
    pub work_dir: Option<PathBuf>,
    /// `--gpu` (`none` or architectures).
    pub gpu: Option<String>,
    /// `--progress`.
    pub progress: String,
    /// `--dry-run`: print the graph and the keys.
    pub dry_run: bool,
    /// `--engine`: the engine folder (profiles under `dm/games`).
    pub engine: Option<PathBuf>,
    /// `--shaders`: an existing `b-<chain>` library folder.
    pub shaders: Option<PathBuf>,
    /// `--companion-app`: the companion app binary embedded next to the built app (`Contents/MacOS/nxnative`), default:
    /// built from the engine's sources by clang + swiftc (`host::companion`) into `<work>/app/nxnative`.
    pub companion: Option<PathBuf>,
    /// Worker threads, `--threads N`. The default is this Mac's P-core equivalents, `round(P + 0.4 × E)`
    /// (10 on an M2 Max with 8 P and 4 E cores).
    pub threads: usize,
    /// `--load-limit <percent>`: the pipeline archive starts more compiler runs only while CPU and memory use stay
    /// below this share of the machine (default 90).
    pub load_limit: u8,
    /// `--own-front on|off|<verdicts>`: the own front end (default on: the raw tier, and own-identical shaders when verdicts exist.
    /// `NXB_OWN_FRONT=0` turns it off).
    pub own_front: String,
    /// `--also-ios`: the iOS shader libraries too, from the same translation (`<work>/shader/ios/b-<V>`, the NSP
    /// script's layout), in the same shader step.
    pub also_ios: bool,
    /// `--shader-manifest <file>`: the keys the title used, one per line. Their shaders are built first.
    pub manifest: Option<PathBuf>,
    /// `--romfs bundle|separate`.
    pub romfs: Option<String>,
    /// `--prepare-xcode`: a per-game Xcode project in the shared workspace instead of an IPA.
    pub prepare_xcode: bool,
    /// `--framework`: route C, the game's modules in `DmGame.framework`.
    pub framework: bool,
    /// `--shell-framework <folder>`: build only the game's framework for the shell.
    pub shell_framework: Option<PathBuf>,
    /// `--team-id`.
    pub team_id: Option<String>,
    /// `--xcode-workspace`.
    pub xcode_workspace: Option<PathBuf>,
    /// `--env-file`: switches baked into the app (one `KEY=VALUE` per line).
    pub env_file: Option<PathBuf>,
    /// `--seen-shaders`: a partial Mac library (the profile's playtest list and what the logs report missing).
    pub seen_shaders: bool,
    /// `--show-fps` / `--no-show-fps` (default on).
    pub show_fps: Option<bool>,
    /// `--motion` / `--no-motion` (default on).
    pub motion: Option<bool>,
    /// `--keep-extracted` / `--no-keep-extracted` (default on).
    pub keep_extracted: Option<bool>,
    /// `--program N` (0 is the default).
    pub program: Option<u32>,
    /// `--allow-shader-regressions`.
    pub allow_shader_regressions: bool,
    /// `--allow-other-metal`: build with a Metal compiler other than the tested one.
    pub allow_other_metal: bool,
    /// `--profile-out`: where a provisional profile goes (`<folder>/<title ID>/`, default `<work>/profile`).
    pub profile_out: Option<PathBuf>,
    /// `--no-heap-check`: no heap check and no region check (they boot the game on the Mac, which nxb build does not
    /// do): the guessed layout stays, and a game that needs a shader capture builds without it.
    pub no_heap_check: bool,
    /// `--provisional`: survey and guess even when the engine has a profile for the game.
    pub provisional: bool,
    /// `--survey-only`: a game with no profile: write the survey and stop.
    pub survey_only: bool,
    /// `--allow-damaged`: files failing the NCA hash check are written as read and the build goes on.
    pub allow_damaged: bool,
    /// `--partial`: build only the manifest's shaders and flag the output as partial (needs `--shader-manifest`).
    pub partial: bool,
    /// `--nup <file>`: a NUP container (a `.nup` file as the input, or the only one in the folder, is found as well).
    pub nup: Option<PathBuf>,
    /// `--dump <folder>`: a dump folder (the game's files already extracted).
    pub dump: Option<PathBuf>,
    /// `--aoc <package>` (repeatable): DLC packages outside the input folder.
    pub aoc: Vec<PathBuf>,
    /// `--resource-patch <option>=<value|stock>` (repeatable): a title's resource patch with its value, `stock` for none
    /// (`resolution-scale=0.75`).
    pub resource_patches: Vec<(String, String)>,
}

/// `--companion-app` under its old name: compatibility alias for the reference build scripts.
const COMPAT_ALIAS: &str = "--launcher";
const VALUE: &[&str] = &[
    "-k",
    "--keys",
    "--title-id",
    "-p",
    "--package",
    "-o",
    "--out",
    "--app-name",
    "--bundle-id",
    "--platform",
    "--mac-sign",
    "-l",
    "--language",
    "--work-dir",
    "--gpu",
    "--progress",
    "--engine",
    "--shaders",
    "--companion-app",
    COMPAT_ALIAS,
    "--threads",
    "--load-limit",
    "--own-front",
    "--shader-manifest",
    "--nup",
    "--dump",
    "--app",
    "--aoc",
    "--resource-patch",
    "--romfs",
    "--shell-framework",
    "--team-id",
    "--xcode-workspace",
    "--env-file",
    "--program",
    "--profile-out",
];
/// Switches without a value: (name, set on a field). Each has a `--no-` form where the script has one.
fn switch(a: &mut Args, k: &str) -> bool {
    match k {
        "--also-ios" => a.also_ios = true,
        "--allow-damaged" => a.allow_damaged = true,
        "--partial" => a.partial = true,
        "--dry-run" => a.dry_run = true,
        "--prepare-xcode" => a.prepare_xcode = true,
        "--framework" => a.framework = true,
        "--allow-shader-regressions" => a.allow_shader_regressions = true,
        "--allow-other-metal" => a.allow_other_metal = true,
        "--no-heap-check" => a.no_heap_check = true,
        "--provisional" => a.provisional = true,
        "--survey-only" => a.survey_only = true,
        "--seen-shaders" => a.seen_shaders = true,
        "--no-seen-shaders" => a.seen_shaders = false,
        "--show-fps" => a.show_fps = Some(true),
        "--no-show-fps" => a.show_fps = Some(false),
        "--motion" => a.motion = Some(true),
        "--no-motion" => a.motion = Some(false),
        "--keep-extracted" => a.keep_extracted = Some(true),
        "--no-keep-extracted" => a.keep_extracted = Some(false),
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

/// The script's checks of option combinations (exit 2).
fn validate(a: &Args) -> Result<(), (i32, String)> {
    let bad = |m: &str| Err((2, m.to_string()));
    let mac = a.platform != "ios";
    let mac_pack = a.platform == "mac-app";
    let platform = &a.platform;
    if let Some(r) = &a.romfs
        && r != "bundle"
        && r != "separate"
    {
        return Err((
            2,
            format!("argument --romfs: invalid choice: '{r}' (choose from bundle, separate)"),
        ));
    }
    if let Some(g) = a.gpu.as_deref().map(str::trim).filter(|g| !g.is_empty())
        && !gpu_list(g)
    {
        return bad(&format!(
            "--gpu takes GPU architectures such as g14s or g18p,g17p, or none: {g}"
        ));
    }
    if a.seen_shaders && !mac {
        return bad(
            "--seen-shaders builds a partial library of Mac shaders: it needs --platform mac or mac-app",
        );
    }
    if a.mac_sign != "-" && !mac_pack {
        return bad("--mac-sign signs the Mac app: it needs --platform mac-app");
    }
    if a.shell_framework.is_some() && (mac || a.prepare_xcode || a.framework) {
        return bad(
            "--shell-framework builds only the game framework for the shell: it cannot be combined with --platform mac or mac-app, --prepare-xcode or --framework",
        );
    }
    if a.shell_framework.is_some() && a.romfs.as_deref() == Some("bundle") {
        return bad(
            "--shell-framework keeps the game files separate: it cannot be combined with --romfs bundle",
        );
    }
    if a.prepare_xcode && mac {
        return bad(&format!(
            "--prepare-xcode makes an iPhone project: it cannot be combined with --platform {platform}"
        ));
    }
    if a.xcode_workspace.is_some() && !a.prepare_xcode {
        return bad("--xcode-workspace requires --prepare-xcode");
    }
    let separate = a.romfs.as_deref() == Some("separate") || a.shell_framework.is_some();
    if separate && mac {
        return bad(&format!(
            "--romfs separate keeps the game files off the iPhone app: it cannot be combined with --platform {platform}"
        ));
    }
    let keep = a.keep_extracted.unwrap_or(true) || a.shell_framework.is_some();
    if a.prepare_xcode && !keep {
        return bad(
            "--prepare-xcode requires --keep-extracted: Xcode builds from the extracted files",
        );
    }
    if separate && !keep {
        return bad(
            "--romfs separate requires --keep-extracted: the image must remain available for the one-time transfer to the phone",
        );
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
        platform: "ios".into(),
        mac_sign: "-".into(),
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
                "--dump" => a.dump = Some(v.into()),
                "--aoc" => a.aoc.push(v.into()),
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
                "--romfs" => a.romfs = Some(v),
                "--shell-framework" => a.shell_framework = Some(v.into()),
                "--team-id" => a.team_id = Some(v),
                "--xcode-workspace" => a.xcode_workspace = Some(v.into()),
                "--env-file" => a.env_file = Some(v.into()),
                "--profile-out" => a.profile_out = Some(v.into()),
                "--program" => {
                    a.program = Some(v.parse().map_err(|_| {
                        (2, format!("argument --program: invalid int value: '{v}'"))
                    })?);
                }
                "-p" | "--package" => a.package = Some(v),
                "-o" | "--out" => a.out = Some(v.into()),
                "--app-name" => a.app_name = Some(v),
                "--bundle-id" => a.bundle_id = Some(v),
                "--platform" => a.platform = v,
                "--mac-sign" => a.mac_sign = v,
                "-l" | "--language" => a.language = Some(v),
                "--work-dir" => a.work_dir = Some(v.into()),
                "--gpu" => a.gpu = Some(v),
                "--progress" => a.progress = v,
                "--engine" => a.engine = Some(v.into()),
                "--shaders" => a.shaders = Some(v.into()),
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
                _ if k == "--companion-app" || k == COMPAT_ALIAS => a.companion = Some(v.into()),
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
    if !["ios", "mac", "mac-app"].contains(&a.platform.as_str()) {
        return Err((
            2,
            format!(
                "argument --platform: invalid choice: '{}' (choose from ios, mac, mac-app)",
                a.platform
            ),
        ));
    }
    validate(&a)?;
    if a.partial && a.manifest.is_none() {
        return Err((2, "--partial needs --shader-manifest <file>".into()));
    }
    if a.nsp_dir.as_os_str().is_empty() {
        match (&a.nup, &a.dump) {
            (Some(n), None) => a.nsp_dir = n.clone(),
            (None, Some(d)) => a.nsp_dir = d.clone(),
            (Some(_), Some(_)) => return Err((2, "--nup and --dump exclude each other".into())),
            (None, None) => {
                return Err((
                    2,
                    "the input is required: a folder of packages, a .nup file, a dump folder"
                        .into(),
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
    fn command_line_as_the_nsp_script() {
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
        assert!(parse(&v(&["dir", "--platform", "mac-app", "--romfs", "bundle"])).is_ok());
        let code = |a: &[&str]| parse(&v(a)).err().map(|e| e.0);
        // combinations the script refuses: 2
        let a = parse(&v(&[
            "dir",
            "--provisional",
            "--survey-only",
            "--no-heap-check",
        ]))
        .unwrap();
        assert!(a.provisional && a.survey_only && a.no_heap_check);
        assert_eq!(
            parse(&v(&["dir", "--program", "2"])).map(|a| a.program),
            Ok(Some(2))
        );
        assert_eq!(code(&["dir", "--program", "x"]), Some(2));
        assert_eq!(parse(&v(&["dir"])).map(|a| a.load_limit), Ok(90));
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
        assert_eq!(code(&["dir", "--program", "-1"]), Some(2));
        assert_eq!(
            code(&["dir", "--platform", "mac-app", "--romfs", "separate"]),
            Some(2)
        );
        assert_eq!(code(&["dir", "--romfs", "half"]), Some(2));
        assert_eq!(code(&["dir", "--gpu", "m2"]), Some(2));
        assert!(parse(&v(&["dir", "--gpu", "g14s,applegpu_g18p"])).is_ok());
        assert!(parse(&v(&["dir", "--gpu", "none"])).is_ok());
        assert_eq!(code(&["dir", "--seen-shaders"]), Some(2));
        assert_eq!(code(&["dir", "--mac-sign", "ID"]), Some(2));
        assert_eq!(
            code(&["dir", "--xcode-workspace", "w.xcworkspace"]),
            Some(2)
        );
        assert_eq!(
            code(&["dir", "--shell-framework", "f", "--framework"]),
            Some(2)
        );
        assert_eq!(
            code(&["dir", "--prepare-xcode", "--no-keep-extracted"]),
            Some(2)
        );
        let a = parse(&v(&[
            "dir",
            "--platform",
            "mac-app",
            "--no-motion",
            "--seen-shaders",
            "--mac-sign",
            "ID",
            "--env-file",
            "e.txt",
        ]))
        .unwrap();
        assert!(
            a.motion == Some(false) && a.seen_shaders && a.mac_sign == "ID" && a.env_file.is_some()
        );
        assert_eq!(
            parse(&v(&["dir", "--platform", "mac-app", "--nope"]))
                .err()
                .map(|e| e.0),
            Some(2)
        );
        assert!(parse(&v(&["dir", "--platform", "mac", "--seen-shaders"])).is_ok());
        assert_eq!(
            code(&["dir", "--platform", "mac", "--mac-sign", "ID"]),
            Some(2)
        );
        assert_eq!(
            code(&["dir", "--platform", "mac", "--prepare-xcode"]),
            Some(2)
        );
        assert_eq!(
            code(&["dir", "--platform", "mac", "--romfs", "separate"]),
            Some(2)
        );
        assert_eq!(
            parse(&v(&["dir", "--platform", "tv"])).err().map(|e| e.0),
            Some(2)
        );
        // manifest-first and partial
        let a = parse(&v(&["dir", "--shader-manifest", "m.txt", "--partial"])).unwrap();
        assert!(a.partial && a.manifest.as_deref() == Some(std::path::Path::new("m.txt")));
        assert_eq!(parse(&v(&["dir", "--partial"])).err().map(|e| e.0), Some(2));
        assert!(parse(&v(&["dir"])).is_ok_and(|a| !a.partial && a.manifest.is_none()));
        // the script's default platform is ios
        assert!(parse(&v(&["dir"])).is_ok_and(|a| a.platform == "ios"));
    }
}
