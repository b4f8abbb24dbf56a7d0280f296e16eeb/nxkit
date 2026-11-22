//! Shader front ends behind one trait (design-v2 16): Apple's `metal -c` (the default) and the optional own front end
//! (`airc` from layer F, then Apple's back half), with a per-shader deny list and a per-shader fallback.
//!
//! Status: designed and unit-tested, **not wired into any default**. The own path is not byte-identical to Apple's by
//! design (design-v2 16.1), it ships only behind the validation gates of design-v2 16.3, so nothing selects it unless a
//! caller builds a [`WithFallback`] around it explicitly.
//!
//! Own path, as the front-end tests validate it (`tools/gpudiff_own.py`, `OWN_OPT=1`):
//! `airc emit IN.metal -o OUT.ll` → `air-opt -O1 OUT.ll -o X.bc` → `air-opt --discard-value-names X.bc -o X.air` →
//! `metallib X.air -o X.metallib`, every Apple tool called directly with the target's `SDKROOT`.

use crate::apple::{Io, MetalTools, StepCost};
use crate::env::EnvPolicy;
use crate::process::ProcessRegistry;
use crate::tool::{ProcessTool, Tool, ToolId};
use nxb_error::{ToolError, ToolFailure};
use nxb_model::{Digest, sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Why a front end produced no library for a shader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrontEndError {
    /// The front end declines the shader (deny list, construct not covered, lowering error): the fallback takes it,
    /// and it is never a shader failure by itself (`ErrorClass::FrontEndReject`).
    Reject(String),
    /// A tool failed, with a fallback this is also handed over, without one it is the shader's failure.
    Tool(ToolError),
}

impl std::fmt::Display for FrontEndError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrontEndError::Reject(r) => write!(f, "front end rejected: {r}"),
            FrontEndError::Tool(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FrontEndError {}

/// A finished library and what it cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontEndOutput {
    /// The metallib bytes.
    pub metallib: Vec<u8>,
    /// Which front end made it ([`FrontEnd::id`]). It is part of TOOL_FP and of the cache key (design-v2 6 rule 8).
    pub frontend: String,
    /// The steps' costs, in order.
    pub steps: Vec<StepCost>,
}

/// MSL in, metallib out, for one target and compile convention.
pub trait FrontEnd: Send + Sync {
    /// Identity for TOOL_FP and cache keys: `apple` or `own@<digest of the airc binary>`.
    fn id(&self) -> &str;

    /// Compile one shader.
    fn compile(&self, msl: &[u8]) -> Result<FrontEndOutput, FrontEndError>;
}

/// Shaders the own front end must not compile, by SHA-256 of their MSL: one lower-case hex digest per line, `#` comments.
/// The list is verdict data (design-v2 16.4): in V, maintained by `fe-verify` data PRs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DenyList {
    digests: BTreeSet<Digest>,
}

impl DenyList {
    /// Parse the text form, bad lines are errors (a typo must not silently allow a shader).
    pub fn parse(text: &str) -> Result<DenyList, String> {
        let mut digests = BTreeSet::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let d = Digest::from_hex(line).map_err(|e| format!("deny list line {}: {e}", n + 1))?;
            digests.insert(d);
        }
        Ok(DenyList { digests })
    }

    /// Read a deny list file.
    pub fn load(path: &Path) -> Result<DenyList, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text)
    }

    /// True when the shader with this MSL is denied.
    pub fn denies(&self, msl: &[u8]) -> bool {
        self.digests.contains(&sha256(msl))
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.digests.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.digests.is_empty()
    }
}

/// Apple's front end: `metal -c <flags> [-include-pch]` and `metallib` ([`MetalTools`]).
#[derive(Debug)]
pub struct AppleFrontEnd {
    tools: Arc<MetalTools>,
    flags: Vec<&'static str>,
    pch: Option<PathBuf>,
    io: Io,
}

impl AppleFrontEnd {
    /// Apple's front end with a convention's flags and an optional precompiled header.
    pub fn new(
        tools: Arc<MetalTools>,
        flags: &[&'static str],
        pch: Option<PathBuf>,
        io: Io,
    ) -> Self {
        AppleFrontEnd {
            tools,
            flags: flags.to_vec(),
            pch,
            io,
        }
    }
}

impl FrontEnd for AppleFrontEnd {
    fn id(&self) -> &str {
        "apple"
    }

    fn compile(&self, msl: &[u8]) -> Result<FrontEndOutput, FrontEndError> {
        let (air, c) = self
            .tools
            .compile(&self.flags, self.pch.as_deref(), msl, self.io)
            .map_err(FrontEndError::Tool)?;
        let (lib, l) = self
            .tools
            .link(&air, self.io)
            .map_err(FrontEndError::Tool)?;
        Ok(FrontEndOutput {
            metallib: lib,
            frontend: "apple".into(),
            steps: vec![c, l],
        })
    }
}

/// Paths of the own front end's tools.
#[derive(Clone, Debug)]
pub struct OwnToolPaths {
    /// `airc` (layer F, `crates/airc`).
    pub airc: PathBuf,
    /// Apple's `air-opt` (`xcrun -sdk <sdk> -find air-opt`).
    pub air_opt: PathBuf,
    /// Apple's `metallib`.
    pub metallib: PathBuf,
}

/// The own front end: `airc emit`, then `air-opt -O1`, `air-opt --discard-value-names`, `metallib`.
#[derive(Debug)]
pub struct OwnFrontEnd {
    id: String,
    airc: ProcessTool,
    air_opt: ProcessTool,
    metallib: ProcessTool,
    env: EnvPolicy,
    deny: DenyList,
    tmp: PathBuf,
    timeout: Duration,
    n: AtomicU64,
}

impl OwnFrontEnd {
    /// Build from tool paths. `env` is the target's Apple environment (`SDKROOT` included), the id carries the digest
    /// of the `airc` binary, so a rebuilt front end never shares cache entries with an older one.
    pub fn new(
        paths: OwnToolPaths,
        env: EnvPolicy,
        deny: DenyList,
        tmp: PathBuf,
        registry: Arc<ProcessRegistry>,
    ) -> Result<Self, String> {
        let bin = fs::read(&paths.airc).map_err(|e| format!("{}: {e}", paths.airc.display()))?;
        let digest = sha256(&bin).to_hex();
        let tool = |name: &str, path: PathBuf| {
            ProcessTool::new(
                ToolId {
                    name: name.to_string(),
                    path,
                    version: String::new(),
                },
                registry.clone(),
            )
        };
        Ok(OwnFrontEnd {
            id: format!("own@{}", &digest[..16]),
            airc: tool("airc", paths.airc),
            air_opt: tool("air-opt", paths.air_opt),
            metallib: tool("metallib", paths.metallib),
            env,
            deny,
            tmp,
            timeout: Duration::from_secs(10 * 60),
            n: AtomicU64::new(0),
        })
    }

    fn temp(&self, ext: &str) -> PathBuf {
        let n = self.n.fetch_add(1, Ordering::Relaxed);
        self.tmp
            .join(format!("own-{}-{n}.{ext}", std::process::id()))
    }

    fn step(&self, t: &ProcessTool, args: &[&Path], flags: &[&str]) -> Result<StepCost, ToolError> {
        let mut inv = t.invocation().env(self.env.clone()).timeout(self.timeout);
        inv = inv.args(flags.iter().copied());
        for a in args {
            inv = inv.arg(a);
        }
        let c = t.run(&inv)?;
        Ok(StepCost {
            wall: c.wall,
            cpu: c.usage.user + c.usage.system,
            max_rss: c.usage.max_rss,
        })
    }

    fn run_steps(
        &self,
        src: &Path,
        ll: &Path,
        bc: &Path,
        air: &Path,
        lib: &Path,
    ) -> Result<Vec<StepCost>, FrontEndError> {
        let mut steps = Vec::new();
        // The lowering's own rejection (exit status of `airc emit`) is a Reject, not a tool failure.
        match self.step(&self.airc, &[src, Path::new("-o"), ll], &["emit"]) {
            Ok(c) => steps.push(c),
            Err(e) if matches!(e.failure, ToolFailure::Exit(_)) => {
                let why = e
                    .stderr_head
                    .lines()
                    .next()
                    .unwrap_or("lowering failed")
                    .to_string();
                return Err(FrontEndError::Reject(why));
            }
            Err(e) => return Err(FrontEndError::Tool(e)),
        }
        let s = self
            .step(&self.air_opt, &[ll, Path::new("-o"), bc], &["-O1"])
            .map_err(FrontEndError::Tool)?;
        steps.push(s);
        let s = self
            .step(
                &self.air_opt,
                &[bc, Path::new("-o"), air],
                &["--discard-value-names"],
            )
            .map_err(FrontEndError::Tool)?;
        steps.push(s);
        let s = self
            .step(&self.metallib, &[air, Path::new("-o"), lib], &[])
            .map_err(FrontEndError::Tool)?;
        steps.push(s);
        Ok(steps)
    }
}

impl FrontEnd for OwnFrontEnd {
    fn id(&self) -> &str {
        &self.id
    }

    fn compile(&self, msl: &[u8]) -> Result<FrontEndOutput, FrontEndError> {
        if self.deny.denies(msl) {
            return Err(FrontEndError::Reject("on the deny list".into()));
        }
        let [src, ll, bc, air, lib] =
            ["metal", "ll", "bc", "air", "metallib"].map(|e| self.temp(e));
        let res = fs::write(&src, msl)
            .map_err(|e| FrontEndError::Tool(ToolError::new("airc", ToolFailure::Io(e.kind()))))
            .and_then(|()| self.run_steps(&src, &ll, &bc, &air, &lib));
        let bytes = res.as_ref().ok().map(|_| fs::read(&lib));
        for p in [&src, &ll, &bc, &air, &lib] {
            let _ = fs::remove_file(p);
        }
        let steps = res?;
        let metallib = bytes.unwrap_or_else(|| Ok(Vec::new())).map_err(|e| {
            FrontEndError::Tool(ToolError::new("metallib", ToolFailure::Io(e.kind())))
        })?;
        Ok(FrontEndOutput {
            metallib,
            frontend: self.id.clone(),
            steps,
        })
    }
}

/// The per-shader fallback (design-v2 16.4): the primary (own) front end first, a rejection or a tool failure hands
/// the shader to the fallback (Apple). The output says which one made the library.
pub struct WithFallback {
    primary: Box<dyn FrontEnd>,
    fallback: Box<dyn FrontEnd>,
    id: String,
    fallbacks: AtomicU64,
}

impl std::fmt::Debug for WithFallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WithFallback")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl WithFallback {
    /// Primary with fallback.
    pub fn new(primary: Box<dyn FrontEnd>, fallback: Box<dyn FrontEnd>) -> Self {
        let id = format!("{}+{}", primary.id(), fallback.id());
        WithFallback {
            primary,
            fallback,
            id,
            fallbacks: AtomicU64::new(0),
        }
    }

    /// Shaders handed to the fallback so far (a metric, reported by reason in the build metrics later).
    pub fn fallbacks(&self) -> u64 {
        self.fallbacks.load(Ordering::Relaxed)
    }
}

impl FrontEnd for WithFallback {
    fn id(&self) -> &str {
        &self.id
    }

    fn compile(&self, msl: &[u8]) -> Result<FrontEndOutput, FrontEndError> {
        match self.primary.compile(msl) {
            Ok(o) => Ok(o),
            Err(_) => {
                self.fallbacks.fetch_add(1, Ordering::Relaxed);
                self.fallback.compile(msl)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(&'static str, bool);

    impl FrontEnd for Fake {
        fn id(&self) -> &str {
            self.0
        }
        fn compile(&self, msl: &[u8]) -> Result<FrontEndOutput, FrontEndError> {
            if self.1 && msl.starts_with(b"reject") {
                return Err(FrontEndError::Reject("not covered".into()));
            }
            Ok(FrontEndOutput {
                metallib: msl.to_vec(),
                frontend: self.0.into(),
                steps: Vec::new(),
            })
        }
    }

    #[test]
    fn deny_list_parse_and_lookup() {
        let d = sha256(b"bad shader").to_hex();
        let list = DenyList::parse(&format!("# header\n{d}  # why\n\n")).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list.denies(b"bad shader"));
        assert!(!list.denies(b"good shader"));
        assert!(DenyList::parse("not-a-digest\n").is_err());
        assert!(DenyList::parse("").unwrap().is_empty());
    }

    #[test]
    fn fallback_takes_rejected_shaders() {
        let f = WithFallback::new(
            Box::new(Fake("own@x", true)),
            Box::new(Fake("apple", false)),
        );
        assert_eq!(f.id(), "own@x+apple");
        assert_eq!(f.compile(b"fine").unwrap().frontend, "own@x");
        assert_eq!(f.compile(b"reject me").unwrap().frontend, "apple");
        assert_eq!(f.fallbacks(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn own_front_end_deny_list_and_reject() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nxb-own-fe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // A fake airc that rejects everything with a message, as a lowering error does.
        let airc = dir.join("airc");
        fs::write(
            &airc,
            "#!/bin/sh\necho 'unsupported construct: atomic' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&airc, fs::Permissions::from_mode(0o755)).unwrap();
        let paths = OwnToolPaths {
            airc,
            air_opt: "/nonexistent/air-opt".into(),
            metallib: "/nonexistent/metallib".into(),
        };
        let deny = DenyList::parse(&sha256(b"denied").to_hex()).unwrap();
        let env = EnvPolicy::empty().set("PATH", "/usr/bin:/bin").unwrap();
        let fe = OwnFrontEnd::new(
            paths,
            env,
            deny,
            dir.clone(),
            Arc::new(ProcessRegistry::new()),
        )
        .unwrap();
        assert!(fe.id().starts_with("own@"));
        assert_eq!(
            fe.compile(b"denied"),
            Err(FrontEndError::Reject("on the deny list".into()))
        );
        assert_eq!(
            fe.compile(b"kernel void f() {}"),
            Err(FrontEndError::Reject(
                "unsupported construct: atomic".into()
            ))
        );
        // No temp files left behind (only the fake tool).
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
