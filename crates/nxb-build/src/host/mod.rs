//! The tools the build starts (`xcrun clang` for the GPU probe): every child runs through [`nxb_tool::Tool`] (own
//! process group, exact environment, timeout).

pub mod gpu;

use nxb_tool::{Completed, EnvPolicy, Invocation, ProcessRegistry, ProcessTool, Tool, ToolId};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The folders put in front of `PATH` (`TOOL_PATHS`: GUI apps do not get the shell's `PATH`).
pub const TOOL_PATHS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Runs the stages' tools: one registry (cancellation) and one environment for all of them.
#[derive(Clone, Debug)]
pub struct Runner {
    reg: Arc<ProcessRegistry>,
    env: EnvPolicy,
    path: String,
}

impl Runner {
    /// A runner with this environment (`PATH` in it is the search path for programs given by name).
    pub fn new(env: EnvPolicy) -> Self {
        let path = env
            .get("PATH")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/usr/bin:/bin".into());
        Runner {
            reg: Arc::new(ProcessRegistry::new()),
            env,
            path,
        }
    }

    /// The environment for the tools: `PATH` = [`TOOL_PATHS`] + the caller's `PATH` (else
    /// `/usr/bin:/bin:/usr/sbin:/sbin`), and the caller's `HOME`, `TMPDIR`, `USER`, `LANG` when set. The deny list of [`EnvPolicy`] still applies (`DEVELOPER_DIR` never reaches a child).
    pub fn from_parent() -> Result<Self, String> {
        let home = std::env::var("HOME").unwrap_or_default();
        let mut dirs: Vec<String> = TOOL_PATHS
            .iter()
            .map(|d| match d.strip_prefix("~/") {
                Some(rest) if !home.is_empty() => format!("{home}/{rest}"),
                _ => (*d).to_string(),
            })
            .collect();
        dirs.push(std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into()));
        let mut env = EnvPolicy::empty()
            .set("PATH", dirs.join(":"))
            .map_err(|e| e.to_string())?;
        for name in ["HOME", "TMPDIR", "USER", "LANG"] {
            if let Some(v) = std::env::var_os(name) {
                env = env.set(name, v).map_err(|e| e.to_string())?;
            }
        }
        Ok(Runner::new(env))
    }

    /// The same runner with one more variable.
    pub fn with_var(&self, name: &str, value: impl Into<OsString>) -> Result<Self, String> {
        let mut r = self.clone();
        r.env = r.env.set(name, value).map_err(|e| e.to_string())?;
        Ok(r)
    }

    /// The environment the children get.
    pub fn env(&self) -> &EnvPolicy {
        &self.env
    }

    /// The program's absolute path: as given when it has a `/`, else the first match on `PATH`.
    pub fn which(&self, program: &str) -> Result<PathBuf, String> {
        if program.contains('/') {
            return Ok(PathBuf::from(program));
        }
        self.path
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| Path::new(d).join(program))
            .find(|p| p.is_file())
            .ok_or_else(|| format!("{program}: not found on PATH"))
    }

    /// Run one program to completion, a non-zero exit is an error with the end of its output (as `pack_app.py`'s
    /// `run`: `<tool> failed (<code>): <stderr or stdout>`).
    pub fn run<I, S>(&self, program: &str, args: I, cwd: Option<&Path>) -> Result<Completed, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let path = self.which(program)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string());
        let tool = ProcessTool::new(
            ToolId {
                name: name.clone(),
                path,
                version: String::new(),
            },
            self.reg.clone(),
        );
        let mut inv: Invocation = tool
            .invocation()
            .args(args)
            .env(self.env.clone())
            .timeout(Duration::from_secs(60 * 60));
        if let Some(d) = cwd {
            inv = inv.cwd(d);
        }
        tool.run(&inv).map_err(|e| {
            let text = if e.stderr_head.trim().is_empty() {
                e.failure.label()
            } else {
                tail(e.stderr_head.trim(), 1500).to_string()
            };
            format!("{name} failed ({}): {text}", e.failure.label())
        })
    }

    /// [`Runner::run`] and its stdout as text.
    pub fn stdout<I, S>(&self, program: &str, args: I, cwd: Option<&Path>) -> Result<String, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.run(program, args, cwd)
            .map(|c| String::from_utf8_lossy(&c.stdout).into_owned())
    }
}

/// The last `n` bytes of `s` (on a character boundary).
fn tail(s: &str, n: usize) -> &str {
    let mut i = s.len().saturating_sub(n);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

/// A temporary folder inside `parent` (as Python's `tempfile.TemporaryDirectory(dir=parent)`), removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    /// Make a new, empty folder in `parent`.
    pub fn new_in(parent: &Path) -> Result<Self, String> {
        static N: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        for _ in 0..100 {
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = parent.join(format!("tmp-nxb-{}-{n}", std::process::id()));
            match std::fs::create_dir(&p) {
                Ok(()) => return Ok(TempDir(p)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("{}: {e}", p.display())),
            }
        }
        Err(format!("no temporary folder in {}", parent.display()))
    }

    /// Its path.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_reports_failures() {
        let r = Runner::new(EnvPolicy::empty().set("PATH", "/usr/bin:/bin").unwrap());
        assert_eq!(r.stdout("echo", ["hi"], None).unwrap(), "hi\n");
        let e = r
            .run("sh", ["-c", "echo bad >&2; exit 3"], None)
            .unwrap_err();
        assert_eq!(e, "sh failed (exit 3): bad");
        assert!(r.which("no-such-tool-nxb").is_err());
    }

    #[test]
    fn temp_dir_is_removed() {
        let base = std::env::temp_dir().join(format!("nxb-host-tmp-{}", std::process::id()));
        let p = {
            let t = TempDir::new_in(&base).unwrap();
            std::fs::write(t.path().join("x"), b"1").unwrap();
            t.path().to_path_buf()
        };
        assert!(!p.exists());
        let _ = std::fs::remove_dir(&base);
    }
}
