//! The own front end for `build` (on by default, `--own-front off` or `NXB_OWN_FRONT=0` turns it off).
//!
//! A shader whose MSL has an allowed verdict goes through `nxb_tool::airfront` (in-process front, `air-opt`, `metallib`).
//! every other shader, every reject and every own failure goes to Apple's `metal -c` + `metallib`. The cache keys carry
//! [`OwnFront::tag`] when the mode is on, so the two modes never share store entries.
//! The raw tier (`crate::own_raw`) runs first and takes every shader the front end accepts, with its own cache-key
//! field ([`OwnFront::key_field`]).

use nxb_tool::{Io, MetalTools, Resolver, StepCost};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Settings of the own front end.
#[derive(Clone, Debug)]
pub struct OwnFrontConfig {
    /// The JSONL verdict file (`source_sha256`, `verdict`), or a verdicts folder before [`resolve`].
    /// An empty path is no verdict list: the identical path takes no shader and only the raw tier runs.
    pub verdicts: PathBuf,
    /// Verdicts that select the own front end (default `own-identical`).
    pub allow: Vec<String>,
}

/// The verdict data shipped with the workspace: `<dir>/<air-front id>/<toolchain tag>.tsv` (`<sha256 of MSL>\t<verdict>`).
pub const DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/own-front");

/// The kill switch: `NXB_OWN_FRONT=0` turns the own front end off whatever the command line says.
pub const KILL_SWITCH: &str = "NXB_OWN_FRONT";

/// The front end's identity (`air_front::id()`), `None` without the `air-front` feature.
pub fn front_id() -> Option<String> {
    #[cfg(feature = "air-front")]
    {
        Some(nxb_tool::airfront::front_id())
    }
    #[cfg(not(feature = "air-front"))]
    {
        None
    }
}

/// The toolchain part of the verdict file name: the first 16 hex digits of SHA-256 of the compiler identity
/// (`metal`/`metallib --version` and the SDK: a new Xcode needs new verdicts).
pub fn toolchain_tag(compiler_id: &str) -> String {
    nxb_model::sha256(compiler_id.as_bytes()).to_hex()[..16].to_string()
}

/// The default mode: own-identical shaders from the shipped verdicts.
pub fn default_config() -> OwnFrontConfig {
    OwnFrontConfig {
        verdicts: PathBuf::from(DATA_DIR),
        allow: vec!["own-identical".into()],
    }
}

/// Settle the mode for this run: the kill switch, then a verdicts folder resolved to the file of this front end and
/// toolchain. Returns the config (or `None`: off) and a note saying why.
pub fn resolve(
    cfg: Option<&OwnFrontConfig>,
    compiler_id: &str,
) -> (Option<OwnFrontConfig>, String) {
    let Some(cfg) = cfg else {
        return (None, "own front end off".into());
    };
    if std::env::var(KILL_SWITCH).is_ok_and(|v| v == "0") {
        return (None, format!("own front end off ({KILL_SWITCH}=0)"));
    }
    let (raw_mode, raw) = crate::own_raw::mode();
    // The verdicts folder holds the identical path's list for this front end and toolchain. The default folder is a
    // path of the build machine and is missing in a release binary. Without a list only the raw tier runs.
    let file = if cfg.verdicts.is_dir() {
        let Some(id) = front_id() else {
            return (
                None,
                "own front end off (built without the air-front feature)".into(),
            );
        };
        cfg.verdicts
            .join(&id)
            .join(format!("{}.tsv", toolchain_tag(compiler_id)))
    } else if cfg.verdicts == Path::new(DATA_DIR) {
        PathBuf::new()
    } else {
        // a verdict file the caller gave: a wrong path fails the build instead of turning the identical path off
        let note = format!(
            "own front end on, verdicts {}; {raw}",
            cfg.verdicts.display()
        );
        return (Some(cfg.clone()), note);
    };
    if file.is_file() {
        let mut c = cfg.clone();
        c.verdicts = file.clone();
        return (
            Some(c),
            format!("own front end on, verdicts {}; {raw}", file.display()),
        );
    }
    if raw_mode.is_none() {
        return (None, format!("own front end off: no verdicts and {raw}"));
    }
    let mut c = cfg.clone();
    c.verdicts = PathBuf::new();
    (Some(c), format!("own front end on, no verdicts; {raw}"))
}

/// The Mac compiler identity as the build computes it (resolves `metal` and `metallib` through xcrun).
pub fn mac_compiler_id(tmp: &Path) -> Result<String, String> {
    use nxb_tool::{ProcessRegistry, SdkRoots};
    let target = nxb_shader::target::MAC.target;
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let registry = Arc::new(ProcessRegistry::new());
    let resolver = Resolver {
        xcrun: PathBuf::from("/usr/bin/xcrun"),
        developer_dir: std::env::var_os("DEVELOPER_DIR"),
        home: home.clone(),
    };
    let roots = SdkRoots::resolve(&resolver, &[target], &registry).map_err(|e| e.to_string())?;
    let env = roots
        .apple_env(target, tmp, &home)
        .map_err(|e| e.to_string())?;
    let tools = MetalTools::resolve(&resolver, target, env, tmp.to_path_buf(), registry.clone())
        .map_err(|e| e.to_string())?;
    let (ver, build) = resolver
        .sdk_version(target, &registry)
        .map_err(|e| e.to_string())?;
    tools.compiler_id(&ver, &build).map_err(|e| e.to_string())
}

/// Turn a validation result (the front end's JSONL) into the shipped form for this front end and toolchain:
/// `<data>/<id>/<toolchain tag>.tsv`, own-identical shaders only, sorted. Returns the file and its line count.
pub fn ship(jsonl: &Path, data: &Path, compiler_id: &str) -> Result<(PathBuf, usize), String> {
    let id = front_id().ok_or("built without the air-front feature")?;
    let text = std::fs::read_to_string(jsonl).map_err(|e| format!("{}: {e}", jsonl.display()))?;
    let mut lines: Vec<String> = Vec::new();
    for l in text.lines() {
        let get = |name: &str| {
            let pat = format!("\"{name}\": \"");
            let a = l.find(&pat)? + pat.len();
            let b = l[a..].find('"')?;
            Some(l[a..a + b].to_string())
        };
        if let (Some(h), Some(v)) = (get("source_sha256"), get("verdict"))
            && v == "own-identical"
            && h.len() == 64
        {
            lines.push(format!("{h}\t{v}\n"));
        }
    }
    lines.sort();
    lines.dedup();
    let dir = data.join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join(format!("{}.tsv", toolchain_tag(compiler_id)));
    std::fs::write(&file, lines.concat()).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok((file, lines.len()))
}

/// The own front end of a run, with its counters.
pub struct OwnFront {
    #[cfg(feature = "air-front")]
    fe: nxb_tool::airfront::AirFrontEnd,
    #[cfg(feature = "air-front")]
    verdicts: nxb_tool::airfront::Verdicts,
    #[cfg(feature = "air-front")]
    raw: Option<crate::own_raw::RawTier>,
    tag: String,
    /// Libraries the own front end made.
    pub own: AtomicU64,
    /// Selected shaders it rejected or failed on (Apple made them).
    pub fallback: AtomicU64,
    /// Summed CPU of the back-half processes of own libraries.
    pub cpu_ns: AtomicU64,
    /// Libraries Apple's `metal -c` + `metallib` made (no own tier took them or the own tier fell back).
    pub apple: AtomicU64,
}

impl OwnFront {
    /// Load the verdicts and resolve `air-opt` next to `metal`.
    #[cfg(feature = "air-front")]
    pub fn open(
        cfg: &OwnFrontConfig,
        tools: &Arc<MetalTools>,
        resolver: &Resolver,
    ) -> Result<OwnFront, String> {
        let allow: Vec<&str> = cfg.allow.iter().map(String::as_str).collect();
        let text = if cfg.verdicts.as_os_str().is_empty() {
            Vec::new()
        } else {
            std::fs::read(&cfg.verdicts).map_err(|e| format!("{}: {e}", cfg.verdicts.display()))?
        };
        let verdicts =
            nxb_tool::airfront::Verdicts::parse(&String::from_utf8_lossy(&text), &allow)?;
        let air_opt = tools
            .sibling(resolver, nxb_shader::target::MAC.target, "air-opt")
            .map_err(|e| e.to_string())?;
        let fe = nxb_tool::airfront::AirFrontEnd::new(tools.clone(), air_opt, false);
        let tag = format!(
            "{}|{}|{}",
            nxb_tool::FrontEnd::id(&fe),
            cfg.allow.join(","),
            nxb_model::sha256(&text).to_hex()
        );
        Ok(OwnFront {
            fe,
            verdicts,
            raw: crate::own_raw::RawTier::open()?,
            tag,
            own: AtomicU64::new(0),
            fallback: AtomicU64::new(0),
            cpu_ns: AtomicU64::new(0),
            apple: AtomicU64::new(0),
        })
    }

    /// Without the `air-front` feature the mode cannot be turned on.
    #[cfg(not(feature = "air-front"))]
    pub fn open(_: &OwnFrontConfig, _: &Arc<MetalTools>, _: &Resolver) -> Result<OwnFront, String> {
        Err("this build has no own front end (cargo feature air-front)".into())
    }

    /// The cache-key field of the mode: front-end id, allowed verdicts, digest of the verdict file.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The cache-key field of one shader: `own-raw@<air-front id>` when the raw tier takes it, else [`Self::tag`].
    pub fn key_field(&self, msl: &[u8]) -> &str {
        #[cfg(feature = "air-front")]
        if let Some(r) = &self.raw
            && r.takes(msl)
        {
            return r.id();
        }
        let _ = msl;
        &self.tag
    }

    /// The cache-key field of a shader that may compile any of `srcs` (a side unit, then the plain MSL): [`Self::tag`]
    /// when the raw tier takes none of them (the key stays as without the raw tier), else every source's field.
    pub fn key_field_of(&self, srcs: &[&[u8]]) -> String {
        let fields: Vec<&str> = srcs.iter().map(|s| self.key_field(s)).collect();
        if fields.iter().all(|f| *f == self.tag) {
            self.tag.clone()
        } else {
            fields.join("+")
        }
    }

    /// Libraries made by the own front end and fallbacks to Apple.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.own.load(Ordering::Relaxed),
            self.fallback.load(Ordering::Relaxed),
        )
    }

    /// The run note extended with the counters of the tiers.
    pub fn note(&self, base: &str) -> String {
        format!("{base}; {}", self.describe())
    }

    /// Number of allowed shaders in the verdict file, and the verdict counts.
    pub fn describe(&self) -> String {
        #[cfg(feature = "air-front")]
        {
            let raw = self.raw.as_ref().map_or_else(
                || "off".to_string(),
                |r| {
                    let (l, f) = (
                        r.libs.load(Ordering::Relaxed),
                        r.fallback.load(Ordering::Relaxed),
                    );
                    let scope = r
                        .listed()
                        .map_or_else(|| "every shader".to_string(), |n| format!("{n} listed"));
                    format!("{scope}, {l} libraries, {f} fallbacks")
                },
            );
            format!(
                "{} allowed of {:?}; raw tier {raw}; Apple made {} libraries",
                self.verdicts.len(),
                self.verdicts.counts,
                self.apple.load(Ordering::Relaxed)
            )
        }
        #[cfg(not(feature = "air-front"))]
        {
            String::new()
        }
    }

    /// The library when the own front end takes this shader, `None` when Apple must.
    fn try_compile(&self, msl: &[u8]) -> Option<(Vec<u8>, StepCost)> {
        #[cfg(feature = "air-front")]
        {
            if let Some((lib, c)) = self.raw.as_ref().and_then(|r| r.compile(msl)) {
                self.own.fetch_add(1, Ordering::Relaxed);
                let ns = u64::try_from(c.cpu.as_nanos()).unwrap_or(u64::MAX);
                self.cpu_ns.fetch_add(ns, Ordering::Relaxed);
                return Some((lib, c));
            }
            if !self.verdicts.allows(msl) {
                return None;
            }
            match nxb_tool::FrontEnd::compile(&self.fe, msl) {
                Ok(out) => {
                    self.own.fetch_add(1, Ordering::Relaxed);
                    let mut c = StepCost::default();
                    for s in &out.steps {
                        c.wall += s.wall;
                        c.cpu += s.cpu;
                        c.max_rss = c.max_rss.max(s.max_rss);
                    }
                    let ns = u64::try_from(c.cpu.as_nanos()).unwrap_or(u64::MAX);
                    self.cpu_ns.fetch_add(ns, Ordering::Relaxed);
                    Some((out.metallib, c))
                }
                Err(_) => {
                    self.fallback.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        }
        #[cfg(not(feature = "air-front"))]
        {
            let _ = (msl, Ordering::Relaxed);
            None
        }
    }
}

/// Compile and link one source: the own front end when it takes the shader, else `metal -c` + `metallib`.
/// Returns (library, compile cost, link cost), for an own library the whole back half counts as compile.
pub fn compile_link(
    tools: &MetalTools,
    pch: Option<&Path>,
    own: Option<&OwnFront>,
    flags: &[&str],
    src: &[u8],
    io: Io,
) -> Result<(Vec<u8>, StepCost, StepCost), String> {
    if let Some((lib, c)) = own.and_then(|o| o.try_compile(src)) {
        return Ok((lib, c, StepCost::default()));
    }
    if let Some(o) = own {
        o.apple.fetch_add(1, Ordering::Relaxed);
    }
    let (air, c) = tools
        .compile(flags, pch, src, io)
        .map_err(|e| format!("metal failed: {e}"))?;
    let (lib, l) = tools
        .link(&air, io)
        .map_err(|e| format!("metallib failed: {e}"))?;
    Ok((lib, c, l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_verdicts_only_the_raw_tier_runs() {
        if Path::new(DATA_DIR).is_dir() || std::env::var_os(KILL_SWITCH).is_some() {
            eprintln!("skipped: this tree has verdicts or the environment sets {KILL_SWITCH}");
            return;
        }
        let (cfg, note) = resolve(Some(&default_config()), "id");
        match crate::own_raw::mode().0 {
            Some(_) => {
                let cfg = cfg.expect(&note);
                assert!(cfg.verdicts.as_os_str().is_empty(), "{note}");
                assert!(note.starts_with("own front end on, no verdicts"), "{note}");
            }
            None => assert!(cfg.is_none(), "{note}"),
        }
    }

    #[test]
    fn a_given_verdicts_path_stays_on() {
        if std::env::var(KILL_SWITCH).is_ok_and(|v| v == "0") {
            return;
        }
        let c = OwnFrontConfig {
            verdicts: PathBuf::from("/nonexistent/verdicts.tsv"),
            allow: vec!["own-identical".into()],
        };
        let (cfg, _) = resolve(Some(&c), "id");
        assert_eq!(cfg.map(|c| c.verdicts), Some(c.verdicts));
    }
}
