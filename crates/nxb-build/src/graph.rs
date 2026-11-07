//! The stage graph of one build: inputs discovered as the NSP script finds them, one node per stage with its
//! dependencies and cache key, run level by level (nodes of one level at once).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nxb_model::ContentKey;

use crate::Args;
use crate::events::Progress;

/// The game profile `dir/game.tsv`: key -> values. A provisional profile that the survey has not written yet reads
/// as no keys (`missing_ok`).
fn profile_at(dir: &Path, missing_ok: bool) -> Result<(PathBuf, BTreeMap<String, String>), String> {
    let text = match std::fs::read_to_string(dir.join("game.tsv")) {
        Ok(t) => t,
        Err(_) if missing_ok => String::new(),
        Err(e) => return Err(format!("{}: {e}", dir.join("game.tsv").display())),
    };
    let mut m = BTreeMap::new();
    for l in text.lines().filter(|l| !l.starts_with('#')) {
        if let Some((k, v)) = l.split_once('\t') {
            m.entry(k.to_string()).or_insert_with(|| v.to_string());
        }
    }
    Ok((dir.to_path_buf(), m))
}

fn identity(p: &Path) -> String {
    std::fs::metadata(p)
        .map(|m| format!("{}:{}:{}", p.display(), m.len(), m.mtime()))
        .unwrap_or_default()
}

/// One stage of the graph.
#[derive(Debug)]
pub struct Node {
    /// Stage name as the NSP script names it.
    pub name: String,
    /// Indices of the stages it waits for.
    pub deps: Vec<usize>,
    /// Its cache key (inputs).
    pub key: ContentKey,
    /// Done already (reused outputs), and how.
    pub reused: Option<String>,
}

/// Everything a build needs to know before it starts.
#[derive(Debug)]
pub struct Build {
    /// The game's title ID. With `--program N`: the program's (the application's + N).
    pub title: String,
    /// `--program N`: which program of a multi-program application (0: the application's own).
    pub program: u64,
    /// A Mac-only build for development (`--platform mac`): the Mac host and the command that runs the game, no app.
    pub mac_run: bool,
    /// Base NSP, updates (version, path).
    pub base: PathBuf,
    /// Updates.
    pub updates: Vec<(u32, PathBuf)>,
    /// The packed version (`v327680`).
    pub package: String,
    /// The versions the split reads.
    pub split_versions: Vec<String>,
    /// Work folder.
    pub work: PathBuf,
    /// The profile folder and its keys.
    pub profile: (PathBuf, BTreeMap<String, String>),
    /// The shader libraries (`b-<chain>`).
    pub shaders: PathBuf,
    /// The app.
    pub out: PathBuf,
    /// GPU architectures of the pipeline archive (empty: none).
    pub gpus: Vec<String>,
    /// The engine folder.
    pub engine: PathBuf,
    /// The stages.
    pub nodes: Vec<Node>,
    /// What the game is read from.
    pub src: crate::inputs::Inputs,
    /// The profile is a provisional one (`<work>/profile/<title>`, or `--profile-out`): the engine has none for the
    /// game or `--provisional` asked for one.
    pub prov: bool,
    /// The graphics API and why, found in the extract once it is there (`crate::layer::gfx`).
    pub gfx: std::sync::OnceLock<(crate::layer::Gfx, String)>,
}

impl Build {
    /// A build with nothing but a work folder and a provisional profile folder.
    #[cfg(test)]
    pub(crate) fn for_test(work: PathBuf, profile: PathBuf) -> Build {
        Build {
            title: "0400000000000000".into(),
            program: 0,
            mac_run: false,
            base: PathBuf::new(),
            updates: Vec::new(),
            package: "v0".into(),
            split_versions: Vec::new(),
            work,
            profile: (profile, BTreeMap::new()),
            shaders: PathBuf::new(),
            out: PathBuf::new(),
            gpus: Vec::new(),
            engine: PathBuf::new(),
            nodes: Vec::new(),
            src: crate::inputs::Inputs {
                source: crate::inputs::Source::Nup,
                app: 0x0400_0000_0000_0000,
                base: PathBuf::new(),
                updates: Vec::new(),
                dlc: Vec::new(),
                app_flag: None,
                has_base: false,
                notes: Vec::new(),
            },
            prov: true,
            gfx: std::sync::OnceLock::new(),
        }
    }

    /// The shader split runs inside the extract's pass (packages with a profile in place).
    pub(crate) fn split_fused(&self) -> bool {
        self.src.source == crate::inputs::Source::Packages && self.nodes[0].reused.is_none()
    }
}

/// Discovers the inputs and builds the graph.
pub fn plan(a: &Args) -> Result<Build, String> {
    let mut src = crate::inputs::discover(a)?;
    // one program of a multi-program application is a game of its own: its title ID is the application's + N, and the
    // packages are still read as the application
    let program = u64::from(a.program.unwrap_or(0));
    if program != 0 {
        src.app_flag = Some(src.app);
        src.app += program;
    }
    let title = format!("{:016X}", src.app);
    let (base, updates) = (src.base.clone(), src.updates.clone());
    // the folder the build folder and the output default into: the input folder, or the one holding the .nup file
    let home: PathBuf = if a.nsp_dir.is_file() {
        a.nsp_dir
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    } else {
        a.nsp_dir.clone()
    };
    let engine = a
        .engine
        .clone()
        .or_else(|| std::env::var_os("NXB_ENGINE").map(PathBuf::from))
        .ok_or("--engine <engine folder> (or NXB_ENGINE) is required (profiles under dm/games)")?;
    let work = a
        .work_dir
        .clone()
        .unwrap_or_else(|| home.join(format!("{title}_build")));
    let registered = engine.join("dm/games").join(&title);
    let has_registered = registered.join("game.tsv").is_file();
    let prov = a.provisional || !has_registered;
    if !prov && a.survey_only {
        crate::exit::set(2);
        return Err(format!(
            "--survey-only: the engine has a profile for {title}: it surveys a game with no profile (add --provisional to survey it anyway)"
        ));
    }
    let prof = if prov {
        let root = a
            .profile_out
            .as_deref()
            .map_or_else(|| work.join("profile"), crate::options::expand);
        profile_at(&root.join(&title), true)?
    } else {
        profile_at(&registered, false)?
    };
    let newest = updates.iter().map(|u| u.0).max().unwrap_or(0);
    let package = a
        .package
        .clone()
        .or_else(|| prof.1.get("package").cloned())
        .unwrap_or_else(|| format!("v{newest}"));
    let split_versions: Vec<String> = prof
        .1
        .get("shader_versions")
        .map(|v| v.split(' ').map(str::to_string).collect())
        .unwrap_or_else(|| vec![package.clone()]);
    // a version the input lacks has no image: stop here with the reason (exit code 4 where the script derives a profile)
    crate::versions::check(
        &src,
        &work,
        &title,
        &package,
        &split_versions,
        a.package.is_some(),
    )
    .map_err(|(code, text)| crate::exit::fail(code, text))?;
    // the shader step's library folder: given (--shaders: an existing folder, the step is skipped) or the one this
    // build writes, `<work>/shader/cache/b-<V>` with V from the engine (dmsh_chain.py)
    let shaders = match &a.shaders {
        Some(s) => s.clone(),
        None => work
            .join("shader/cache")
            .join(format!("b-{}", nxb_shader::chain::version(&engine)?)),
    };
    let name = a
        .app_name
        .clone()
        .or_else(|| prof.1.get("name").cloned())
        .unwrap_or_else(|| title.clone());
    // a provisional build of a game the engine has a profile for never takes the place of the registered build's app
    let shadow = if prov && has_registered {
        "-provisional"
    } else {
        ""
    };
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| home.join(format!("{name}{shadow}.app")));
    // `--gpu`: architectures, `none`, or by default (a Mac app of a game with shader pairs) this Mac's GPU, as the
    // script resolves it, an iOS build takes its own list from the profile
    let gpus: Vec<String> = match a.gpu.as_deref().map(str::trim) {
        Some("none") => Vec::new(),
        Some(g) if !g.is_empty() => g.split(',').map(nxb_archive::plan::arch_name).collect(),
        _ if a.platform == "mac-app" && prof.0.join("shader_pairs.tsv").is_file() => {
            vec![crate::host::gpu::mac_arch(&work)?]
        }
        _ => Vec::new(),
    };
    let key = |name: &str, fields: &[(&str, String)]| {
        let mut b = ContentKey::builder(&format!("nxb-build {name} 1"));
        for (k, v) in fields {
            b = b.field_str(k, v);
        }
        b.finish()
    };
    let nsps: String = std::iter::once(identity(&base))
        .chain(updates.iter().map(|u| identity(&u.1)))
        .chain(src.dlc.iter().map(|d| identity(&d.2)))
        .collect::<Vec<_>>()
        .join(";");
    let ex = |v: &str| work.join("extract").join(v).join("RomFS.img");
    let have_extract = split_versions
        .iter()
        .chain([&package])
        .all(|v| ex(v).is_file());
    // a provisional build's rewrite is part of its extract stage: the images alone do not make it reusable
    let have_patched = !prov
        || std::fs::read_dir(work.join(format!("patched_{package}"))).is_ok_and(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".patched.bin"))
        });
    let have_split =
        std::fs::read_dir(work.join("shader/pkg")).is_ok_and(|mut d| d.next().is_some());
    // the script names the stage after what it extracts ("extract v0"): the launcher shows "extract %@" with that part
    let mut extracted: Vec<&str> = split_versions.iter().map(String::as_str).collect();
    if !extracted.contains(&package.as_str()) {
        extracted.push(&package);
    }
    let mut nodes = vec![
        Node {
            name: format!("extract {}", extracted.join(" + ")),
            deps: vec![],
            key: key(
                "extract",
                &[
                    ("nsps", nsps.clone()),
                    ("versions", split_versions.join(" ")),
                ],
            ),
            reused: (have_extract && have_patched)
                .then(|| "the work folder's images (kept for the next build)".into()),
        },
        Node {
            name: "shader split".into(),
            deps: vec![0],
            key: key(
                "split",
                &[
                    ("nsps", nsps),
                    ("profile", identity(&prof.0.join("game.tsv"))),
                ],
            ),
            reused: have_split.then(|| "the work folder's package files".into()),
        },
        Node {
            name: "Mac shaders".into(),
            deps: vec![1],
            key: key("shaders", &[("libraries", identity(&shaders))]),
            reused: a
                .shaders
                .as_ref()
                .map(|s| format!("{} (an existing library folder, --shaders)", s.display())),
        },
    ];
    for g in &gpus {
        nodes.push(Node {
            name: format!("Pipeline archive {g}"),
            deps: vec![2],
            key: key(
                "pso",
                &[("arch", g.clone()), ("libraries", identity(&shaders))],
            ),
            reused: None,
        });
    }
    nodes.push(Node {
        name: "Mac host".into(),
        // a provisional profile changes in the extract stage (refine, the heap check): the host is built for its last state
        deps: if prov { vec![0] } else { vec![] },
        key: key("host", &[("work", work.display().to_string())]),
        reused: None,
    });
    let mac_run = a.platform == "mac";
    if !mac_run {
        let app_deps: Vec<usize> = (2..nodes.len()).collect();
        nodes.push(Node {
            name: "Mac app".into(),
            deps: app_deps,
            key: key(
                "app",
                &[
                    ("out", out.display().to_string()),
                    ("package", package.clone()),
                ],
            ),
            reused: None,
        });
    }
    let mut b = Build {
        title,
        program,
        mac_run,
        base,
        updates,
        package,
        split_versions,
        work,
        profile: prof,
        shaders,
        out,
        gpus,
        engine,
        nodes,
        src,
        prov,
        gfx: std::sync::OnceLock::new(),
    };
    if a.platform == "ios" {
        // the IPA is named by the profile, beside the build folder, as the script names it
        if a.out.is_none() {
            let ipa = b
                .profile
                .1
                .get("ipa")
                .cloned()
                .unwrap_or_else(|| format!("{name}.ipa"));
            b.out = b.work.parent().unwrap_or(&home).join(ipa);
        }
        b.gpus = Vec::new();
        b.nodes = crate::iosbuild::nodes(a, &b);
    }
    Ok(b)
}

/// The plan as text (`--dry-run`).
pub fn describe(b: &Build) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "game {} package {} work {}",
        b.title,
        b.package,
        b.work.display()
    );
    let _ = writeln!(s, "input {:?}", b.src.source);
    let _ = writeln!(s, "base {}", b.base.display());
    for (v, p) in &b.updates {
        let _ = writeln!(s, "update v{v} {}", p.display());
    }
    for (t, v, p) in &b.src.dlc {
        let _ = writeln!(s, "dlc {t:016X} v{v} {}", p.display());
    }
    if b.program != 0 {
        let _ = writeln!(s, "program {} of the application", b.program);
    }
    if !b.mac_run {
        let _ = writeln!(s, "app {}", b.out.display());
    }
    for (i, n) in b.nodes.iter().enumerate() {
        let deps: Vec<String> = n.deps.iter().map(|d| b.nodes[*d].name.clone()).collect();
        let _ = writeln!(
            s,
            "stage {i} {:<28} after [{}] key {}{}",
            n.name,
            deps.join(", "),
            &n.key.to_hex()[..16],
            n.reused
                .as_ref()
                .map(|r| format!(" (reused: {r})"))
                .unwrap_or_default()
        );
    }
    let _ = writeln!(
        s,
        "rewrite: Rust when the work folder has no patched_{} and the engine matches, else taken from the work folder",
        b.package
    );
    s
}

/// Runs the build (`crate::stages::run_nodes`. The events stay those of a run one after another). A multi-program
/// application builds its other programs too (`crate::programs`), a Mac-only build ends with the run command.
pub fn run(a: &Args, p: &std::sync::Arc<Progress>) -> Result<(), String> {
    // a title with a title module builds without the engine folder (R49)
    if crate::standalone::run(a, p)? {
        return Ok(());
    }
    let mut b = plan(a)?;
    if a.dry_run {
        print!("{}", describe(&b));
        return Ok(());
    }
    if b.prov {
        // the tools the build starts (make, the Python helpers) find the provisional profile as the script's
        // DM_GAMES_DIR makes them
        crate::host::set_games(
            b.profile
                .0
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            a.provisional
                && b.engine
                    .join("dm/games")
                    .join(&b.title)
                    .join("game.tsv")
                    .is_file(),
        );
        // no profile yet: survey and guess first (they write it), then plan again with it in place
        if crate::provisional::prepare(a, &b, p)? {
            return Ok(());
        }
        b = plan(a)?;
    }
    crate::options::check_metal(a, &b, p)?;
    let seen_args;
    let a = if a.seen_shaders {
        seen_args = crate::options::seen_shaders(a, &b, p)?;
        &seen_args
    } else {
        a
    };
    if a.platform == "ios" {
        return crate::iosbuild::run(a, &b, p);
    }
    let programs = crate::programs::before_app(a, &b, p)?;
    crate::stages::run_nodes(a, &b, p, &programs)?;
    if b.mac_run {
        return crate::mac_run::finish(a, &b, p);
    }
    p.say(&format!("done: {}", b.out.display()));
    Ok(())
}
