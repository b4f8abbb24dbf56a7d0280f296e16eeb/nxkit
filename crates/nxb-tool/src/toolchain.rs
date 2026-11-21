//! Toolchain resolution: tool paths and `SDKROOT` per target, resolved once per run through `xcrun`.
//!
//! The caller's `DEVELOPER_DIR` is honoured here only (and recorded in TOOL_FP), children never receive it (design-v2 9.1).

use crate::env::{EnvPolicy, EnvPolicyError};
use crate::invocation::Invocation;
use crate::process::{ProcessRegistry, run_process};
use nxb_error::{ToolError, ToolFailure};
use nxb_model::Target;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How `xcrun` is called for resolution.
#[derive(Clone, Debug)]
pub struct Resolver {
    /// Path of `xcrun` (normally `/usr/bin/xcrun`).
    pub xcrun: PathBuf,
    /// The caller's `DEVELOPER_DIR`, if set.
    pub developer_dir: Option<std::ffi::OsString>,
    /// The home folder of the current account (xcrun keeps its cache there).
    pub home: PathBuf,
}

impl Resolver {
    /// The resolver of this process: `/usr/bin/xcrun`, the caller's `DEVELOPER_DIR` and `HOME` (`/` without one).
    pub fn from_env() -> Resolver {
        Resolver {
            xcrun: PathBuf::from("/usr/bin/xcrun"),
            developer_dir: std::env::var_os("DEVELOPER_DIR"),
            home: std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from),
        }
    }

    fn env(&self) -> EnvPolicy {
        let mut env = EnvPolicy::empty()
            .set("PATH", "/usr/bin:/bin")
            .and_then(|e| e.set("HOME", self.home.as_os_str()))
            .unwrap_or_default();
        if let Some(d) = &self.developer_dir {
            env = env.set_for_resolution("DEVELOPER_DIR", d);
        }
        env
    }

    fn query(&self, args: &[&OsStr], reg: &ProcessRegistry) -> Result<PathBuf, ToolError> {
        let inv = Invocation::new(&self.xcrun)
            .args(args.iter().copied())
            .env(self.env())
            .timeout(Duration::from_secs(60));
        let out = run_process("xcrun", &inv, reg)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().next().unwrap_or("").trim();
        let path = PathBuf::from(line);
        if line.is_empty() || !path.is_absolute() {
            return Err(ToolError::new("xcrun", ToolFailure::Exit(0))
                .with_stderr(format!("unexpected output {line:?}")));
        }
        Ok(path)
    }

    /// `xcrun -sdk <sdk> --show-sdk-version` and `--show-sdk-build-version` (first lines, trimmed), for the compiler
    /// identity.
    pub fn sdk_version(
        &self,
        target: Target,
        reg: &ProcessRegistry,
    ) -> Result<(String, String), ToolError> {
        let line = |flag: &str| -> Result<String, ToolError> {
            let inv = Invocation::new(&self.xcrun)
                .args(["-sdk", target.sdk(), flag])
                .env(self.env())
                .timeout(Duration::from_secs(60));
            let out = run_process("xcrun", &inv, reg)?;
            Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string())
        };
        Ok((
            line("--show-sdk-version")?,
            line("--show-sdk-build-version")?,
        ))
    }

    /// `xcrun -sdk <sdk> --show-sdk-path` for one target.
    pub fn sdkroot(&self, target: Target, reg: &ProcessRegistry) -> Result<PathBuf, ToolError> {
        self.query(
            &[
                OsStr::new("-sdk"),
                OsStr::new(target.sdk()),
                OsStr::new("--show-sdk-path"),
            ],
            reg,
        )
    }

    /// `xcrun -sdk <sdk> -find <tool>`: the tool's absolute path, so jobs skip `xcrun` (about 6 ms per call).
    pub fn find(
        &self,
        target: Target,
        tool: &str,
        reg: &ProcessRegistry,
    ) -> Result<PathBuf, ToolError> {
        self.query(
            &[
                OsStr::new("-sdk"),
                OsStr::new(target.sdk()),
                OsStr::new("-find"),
                OsStr::new(tool),
            ],
            reg,
        )
    }
}

/// `SDKROOT` per target, resolved once. Every Apple tool job gets its target's value, a missing target is an error,
/// never a silent default.
#[derive(Clone, Debug, Default)]
pub struct SdkRoots {
    roots: BTreeMap<Target, PathBuf>,
}

impl SdkRoots {
    /// Resolve the given targets.
    pub fn resolve(
        resolver: &Resolver,
        targets: &[Target],
        reg: &ProcessRegistry,
    ) -> Result<SdkRoots, ToolError> {
        let mut roots = BTreeMap::new();
        for &t in targets {
            roots.insert(t, resolver.sdkroot(t, reg)?);
        }
        Ok(SdkRoots { roots })
    }

    /// From known paths (tests, toolchain profiles).
    pub fn from_map(roots: BTreeMap<Target, PathBuf>) -> Self {
        SdkRoots { roots }
    }

    /// The target's SDK root.
    pub fn get(&self, t: Target) -> Option<&Path> {
        self.roots.get(&t).map(PathBuf::as_path)
    }

    /// The exact environment for an Apple tool job of target `t`.
    pub fn apple_env(
        &self,
        t: Target,
        tmpdir: &Path,
        home: &Path,
    ) -> Result<EnvPolicy, MissingSdk> {
        let root = self.get(t).ok_or(MissingSdk::NotResolved(t))?;
        EnvPolicy::apple(root, tmpdir, home).map_err(MissingSdk::Policy)
    }
}

/// No environment could be built for an Apple tool job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissingSdk {
    /// The target's SDK root was not resolved.
    NotResolved(Target),
    /// The policy refused a variable.
    Policy(EnvPolicyError),
}

impl std::fmt::Display for MissingSdk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MissingSdk::NotResolved(t) => write!(f, "SDKROOT for target {t} was not resolved"),
            MissingSdk::Policy(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for MissingSdk {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_target_env() {
        let mut m = BTreeMap::new();
        m.insert(Target::Mac, PathBuf::from("/sdk/mac"));
        m.insert(Target::Ios, PathBuf::from("/sdk/ios"));
        let s = SdkRoots::from_map(m);
        let t = Path::new("/t");
        let mac = s.apple_env(Target::Mac, t, t).unwrap();
        let ios = s.apple_env(Target::Ios, t, t).unwrap();
        assert_eq!(mac.get("SDKROOT"), Some(OsStr::new("/sdk/mac")));
        assert_eq!(ios.get("SDKROOT"), Some(OsStr::new("/sdk/ios")));
        assert_eq!(
            s.apple_env(Target::TvOs, t, t),
            Err(MissingSdk::NotResolved(Target::TvOs))
        );
    }

    /// Runs the real `xcrun` on a Mac with Xcode, skipped elsewhere.
    #[cfg(target_os = "macos")]
    #[test]
    fn real_xcrun_when_present() {
        let xcrun = PathBuf::from("/usr/bin/xcrun");
        if !Path::new("/Applications/Xcode.app").exists() || !xcrun.exists() {
            eprintln!("skipped: no Xcode");
            return;
        }
        let r = Resolver {
            xcrun,
            developer_dir: None,
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/".into()),
        };
        let reg = ProcessRegistry::new();
        let roots = SdkRoots::resolve(&r, &[Target::Mac], &reg).unwrap();
        let mac = roots.get(Target::Mac).unwrap();
        assert!(mac.to_string_lossy().contains("MacOSX"), "{mac:?}");
        assert!(mac.exists());
    }
}
