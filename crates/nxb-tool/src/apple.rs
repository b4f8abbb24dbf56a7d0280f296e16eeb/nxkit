//! Adapters for Apple's shader tools: `metal` (compile MSL to AIR, build a precompiled header) and `metallib` (link).
//!
//! The tools are resolved once (`xcrun -sdk <sdk> -find`) and called directly with the exact environment
//! (`EnvPolicy::apple`, `SDKROOT` of the target). The source always goes on stdin (`-x metal -`), never as a file name:
//! clang names a source's static initializer after its file, so a name would change the library (`dmsh_pch.py`).
//! [`Io::Files`] writes the AIR to a temp file like the Python chain. [`Io::Pipes`] streams it (design-v2 9.3).

use crate::env::EnvPolicy;
use crate::invocation::{Completed, Invocation};
use crate::process::ProcessRegistry;
use crate::tool::{ProcessTool, Tool, ToolId};
use crate::toolchain::Resolver;
use nxb_error::{ToolError, ToolFailure};
use nxb_model::Target;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How AIR travels from `metal` to `metallib`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Io {
    /// Temp files (what the Python chain does).
    Files,
    /// stdout of `metal` into stdin of `metallib`.
    Pipes,
}

/// CPU and wall of one tool step, from the child's `wait4`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepCost {
    /// Wall time.
    pub wall: Duration,
    /// User + system CPU of the child.
    pub cpu: Duration,
    /// Peak RSS of the child in bytes.
    pub max_rss: u64,
}

impl StepCost {
    fn of(c: &Completed) -> StepCost {
        StepCost {
            wall: c.wall,
            cpu: c.usage.user + c.usage.system,
            max_rss: c.usage.max_rss,
        }
    }
}

/// `metal` and `metallib` of one target with their environment.
#[derive(Debug)]
pub struct MetalTools {
    metal: ProcessTool,
    metallib: ProcessTool,
    env: EnvPolicy,
    tmp: PathBuf,
    timeout: Duration,
    temp_n: AtomicU64,
}

impl MetalTools {
    /// Resolve `metal` and `metallib` for `target` through `xcrun -find`. `env` is the target's Apple environment
    /// (`SdkRoots::apple_env`).`tmp` a private folder for temp files.
    pub fn resolve(
        resolver: &Resolver,
        target: Target,
        env: EnvPolicy,
        tmp: PathBuf,
        registry: Arc<ProcessRegistry>,
    ) -> Result<MetalTools, ToolError> {
        let id = |name: &str| -> Result<ToolId, ToolError> {
            Ok(ToolId {
                name: name.to_string(),
                path: resolver.find(target, name, &registry)?,
                version: String::new(),
            })
        };
        let (m, l) = (id("metal")?, id("metallib")?);
        Ok(MetalTools {
            metal: ProcessTool::new(m, registry.clone()),
            metallib: ProcessTool::new(l, registry),
            env,
            tmp,
            // design-v2 9.4: an uber shader took 31 s at -O1, kill at 30 min.
            timeout: Duration::from_secs(30 * 60),
            temp_n: AtomicU64::new(0),
        })
    }

    /// The compiler identity as Python's `dmsh_mtlcache.compiler_id`: both tools' `--version` without the
    /// `InstalledDir:` lines (the cryptex mount path changes), then the SDK version and build lines.
    pub fn compiler_id(&self, sdk_version: &str, sdk_build: &str) -> Result<String, ToolError> {
        let mut s = String::new();
        for t in [&self.metal, &self.metallib] {
            let out = t.run(&self.inv(t).arg("--version"))?;
            for line in String::from_utf8_lossy(&out.stdout).split_inclusive('\n') {
                if !line.starts_with("InstalledDir:") {
                    s.push_str(line);
                }
            }
        }
        s.push_str(sdk_version);
        s.push('\n');
        s.push_str(sdk_build);
        s.push('\n');
        Ok(s)
    }

    fn inv(&self, t: &ProcessTool) -> Invocation {
        t.invocation().env(self.env.clone()).timeout(self.timeout)
    }

    fn temp(&self, ext: &str) -> PathBuf {
        let n = self.temp_n.fetch_add(1, Ordering::Relaxed);
        self.tmp
            .join(format!("nxb-{}-{n}.{ext}", std::process::id()))
    }

    /// Build the precompiled header for a convention (`pch_flags`: the compile flags without `-c`) and check it on
    /// `probe`: the AIR with and without the header must be identical, else `Ok(None)` (compile without it, as Python).
    pub fn build_pch(
        &self,
        pch_flags: &[&str],
        compile_flags: &[&str],
        header: &str,
        probe: &str,
        dir: &Path,
    ) -> Result<Option<PathBuf>, ToolError> {
        let h = dir.join("prefix.h");
        let pch = dir.join("prefix.pch");
        fs::write(&h, header).map_err(|e| io_err("metal", e))?;
        let r = self.metal.run(
            &self
                .inv(&self.metal)
                .args(pch_flags.iter().copied())
                .args(["-x", "metal-header"])
                .arg(&h)
                .arg("-o")
                .arg(&pch),
        );
        if r.is_err() {
            return Ok(None);
        }
        let plain = self.compile(compile_flags, None, probe.as_bytes(), Io::Pipes)?;
        let with = self.compile(compile_flags, Some(&pch), probe.as_bytes(), Io::Pipes)?;
        Ok((plain.0 == with.0).then_some(pch))
    }

    /// Compile MSL (on stdin) to AIR. With a header, a failing compile is retried without it, so a failure reports
    /// the plain compile's messages (as `dmsh_pch.compile`).
    pub fn compile(
        &self,
        flags: &[&str],
        pch: Option<&Path>,
        msl: &[u8],
        io: Io,
    ) -> Result<(Vec<u8>, StepCost), ToolError> {
        if let Some(p) = pch
            && let Ok(r) = self.compile_once(flags, Some(p), msl, io)
        {
            return Ok(r);
        }
        self.compile_once(flags, None, msl, io)
    }

    fn compile_once(
        &self,
        flags: &[&str],
        pch: Option<&Path>,
        msl: &[u8],
        io: Io,
    ) -> Result<(Vec<u8>, StepCost), ToolError> {
        let mut inv = self.inv(&self.metal).args(flags.iter().copied());
        if let Some(p) = pch {
            inv = inv.arg("-include-pch").arg(p);
        }
        inv = inv.args(["-x", "metal", "-"]).stdin(msl.to_vec());
        match io {
            Io::Pipes => {
                let c = self.metal.run(&inv.args(["-o", "-"]))?;
                Ok((c.stdout.clone(), StepCost::of(&c)))
            }
            Io::Files => {
                let air = self.temp("air");
                let r = self.metal.run(&inv.arg("-o").arg(&air));
                let bytes = r.as_ref().ok().map(|_| fs::read(&air));
                let _ = fs::remove_file(&air);
                let c = r?;
                let bytes = bytes
                    .unwrap_or_else(|| Ok(Vec::new()))
                    .map_err(|e| io_err("metal", e))?;
                Ok((bytes, StepCost::of(&c)))
            }
        }
    }

    /// Resolve another tool of the same toolchain (`air-opt` for the own front end's back half).
    pub fn sibling(
        &self,
        resolver: &Resolver,
        target: Target,
        name: &str,
    ) -> Result<ProcessTool, ToolError> {
        let registry = self.metal.registry();
        let id = ToolId {
            name: name.to_string(),
            path: resolver.find(target, name, &registry)?,
            version: String::new(),
        };
        Ok(ProcessTool::new(id, registry))
    }

    /// Run a sibling tool with this toolchain's environment: `args`, `stdin` in, stdout back.
    pub fn filter(
        &self,
        t: &ProcessTool,
        args: &[&str],
        stdin: Vec<u8>,
    ) -> Result<(Vec<u8>, StepCost), ToolError> {
        let c = t.run(&self.inv(t).args(args.iter()).stdin(stdin))?;
        let cost = StepCost::of(&c);
        Ok((c.stdout, cost))
    }

    /// Link AIR into a metallib.
    pub fn link(&self, air: &[u8], io: Io) -> Result<(Vec<u8>, StepCost), ToolError> {
        let lib = self.temp("metallib");
        let (r, air_file) = match io {
            Io::Pipes => (
                self.metallib.run(
                    &self
                        .inv(&self.metallib)
                        .arg("-")
                        .arg("-o")
                        .arg(&lib)
                        .stdin(air.to_vec()),
                ),
                None,
            ),
            Io::Files => {
                let a = self.temp("air");
                fs::write(&a, air).map_err(|e| io_err("metallib", e))?;
                (
                    self.metallib
                        .run(&self.inv(&self.metallib).arg(&a).arg("-o").arg(&lib)),
                    Some(a),
                )
            }
        };
        if let Some(a) = air_file {
            let _ = fs::remove_file(a);
        }
        let bytes = r.as_ref().ok().map(|_| fs::read(&lib));
        let _ = fs::remove_file(&lib);
        let c = r?;
        let bytes = bytes
            .unwrap_or_else(|| Ok(Vec::new()))
            .map_err(|e| io_err("metallib", e))?;
        Ok((bytes, StepCost::of(&c)))
    }
}

impl MetalTools {
    /// Resolve the tools of `target` with their environment: the target's `SDKROOT`, the Apple environment
    /// (`SdkRoots::apple_env` with `tmp` and the resolver's home) and `metal` and `metallib`. An error names the step
    /// that failed.
    pub fn for_target(
        resolver: &Resolver,
        target: Target,
        tmp: PathBuf,
        registry: Arc<ProcessRegistry>,
    ) -> Result<MetalTools, String> {
        let roots = crate::SdkRoots::resolve(resolver, &[target], &registry)
            .map_err(|e| format!("SDK: {e}"))?;
        let env = roots
            .apple_env(target, &tmp, &resolver.home)
            .map_err(|e| format!("environment: {e}"))?;
        MetalTools::resolve(resolver, target, env, tmp, registry).map_err(|e| format!("metal: {e}"))
    }

    /// Link several AIR modules into one metallib (the library of a Vulkan game holds one function per shader). The
    /// modules go through temp files: `metallib` reads one module from stdin only.
    pub fn link_many(&self, airs: &[Vec<u8>]) -> Result<(Vec<u8>, StepCost), ToolError> {
        let lib = self.temp("metallib");
        let mut files = Vec::with_capacity(airs.len());
        let mut r = Ok(());
        for air in airs {
            let a = self.temp("air");
            r = fs::write(&a, air).map_err(|e| io_err("metallib", e));
            files.push(a);
            if r.is_err() {
                break;
            }
        }
        let c = r.and_then(|()| {
            self.metallib.run(
                &self
                    .inv(&self.metallib)
                    .args(files.iter())
                    .arg("-o")
                    .arg(&lib),
            )
        });
        for a in &files {
            let _ = fs::remove_file(a);
        }
        let bytes = c.as_ref().ok().map(|_| fs::read(&lib));
        let _ = fs::remove_file(&lib);
        let c = c?;
        let bytes = bytes
            .unwrap_or_else(|| Ok(Vec::new()))
            .map_err(|e| io_err("metallib", e))?;
        Ok((bytes, StepCost::of(&c)))
    }
}

fn io_err(tool: &str, e: std::io::Error) -> ToolError {
    ToolError::new(tool, ToolFailure::Io(e.kind())).with_stderr(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compiles two functions and links them into one library. Skipped where Xcode's Metal tools are missing.
    #[test]
    fn link_many_puts_several_modules_into_one_library() {
        let tmp = std::env::temp_dir().join(format!("nxb-link-many-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        let tools = match MetalTools::for_target(
            &Resolver::from_env(),
            Target::Mac,
            tmp.clone(),
            Arc::new(ProcessRegistry::new()),
        ) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skipped: {e}");
                return;
            }
        };
        let flags = ["-c", "-O1"];
        let air = |name: &str| {
            let src = format!(
                "#include <metal_stdlib>\nkernel void {name}(device float* o [[buffer(0)]]) {{ o[0] = 1.0; }}\n"
            );
            tools
                .compile(&flags, None, src.as_bytes(), Io::Pipes)
                .unwrap()
                .0
        };
        let (a, b) = (air("first_fn"), air("second_fn"));
        let (lib, _) = tools.link_many(&[a, b]).unwrap();
        assert_eq!(&lib[..4], b"MTLB");
        let has = |n: &str| lib.windows(n.len()).any(|w| w == n.as_bytes());
        assert!(has("first_fn") && has("second_fn"));
        // the temp files are gone
        assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0);
        fs::remove_dir_all(&tmp).unwrap();
    }
}
