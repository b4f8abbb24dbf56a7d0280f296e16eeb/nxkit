//! Running a child process: own process group, drained pipes, watchdog, group kill, cancellation.
//!
//! Sequence per job:
//! 1. spawn with the exact environment, in a new process group (pgid = pid), registered in the [`ProcessRegistry`].
//! 2. stdin is written and stdout/stderr are drained on scoped threads, so no pipe can deadlock the child.
//! 3. a waiter thread blocks in `waitid(WNOWAIT)` until the leader exits **without reaping it**.
//! 4. at the deadline the whole group gets `SIGKILL`, after the leader exits, any straggler left in the group is killed
//!    too, so pipes held by grandchildren close and the readers finish.
//! 5. the job leaves the registry, then the leader is reaped. Because the pid stays a zombie until step 5, no group
//!    kill can ever hit a reused pid.

// The helpers are used by the Unix implementation only until Windows Job Objects exist.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use crate::invocation::{Completed, Invocation};
use nxb_error::{EnvError, IoOp, ToolError, ToolFailure};
use std::collections::BTreeSet;
use std::io::{self, Read};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// Stderr kept in a `ToolError` (the circuit breaker and `explain` need only the head).
const STDERR_HEAD: usize = 8 * 1024;

/// While a group is being killed, how often the kill is repeated.
#[cfg(unix)]
const KILL_RECHECK: std::time::Duration = std::time::Duration::from_millis(20);

/// How often a running job looks at the registry's cancel flag (a cancel also kills the group directly).
#[cfg(unix)]
const CANCEL_RECHECK: std::time::Duration = std::time::Duration::from_millis(200);

/// How long to wait for pipes to close after the leader exited before killing the group again.
#[cfg(unix)]
const STRAGGLER_RECHECK: std::time::Duration = std::time::Duration::from_millis(50);

/// The live process groups of this run. [`ProcessRegistry::kill_all`] stops every child at once (interrupt, abort).
#[derive(Debug, Default)]
pub struct ProcessRegistry {
    state: Mutex<RegState>,
}

#[derive(Debug, Default)]
struct RegState {
    live: BTreeSet<u32>,
    cancelled: bool,
}

/// The live groups of every registry in the process, for [`kill_everything`] (a signal handler does not know the registries).
static ALL_LIVE: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// Kills every live process group of every registry in this process (an interrupt of the whole run). Returns how many.
pub fn kill_everything() -> usize {
    let g = ALL_LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    for &pgid in g.iter() {
        let _ = nxb_sys::process::kill_group(pgid, nxb_sys::process::Signal::Kill);
    }
    g.len()
}

impl ProcessRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Mark the run cancelled and kill every live group. Children started later are killed at once.
    pub fn kill_all(&self) {
        let mut g = self.lock();
        g.cancelled = true;
        for &pgid in &g.live {
            let _ = nxb_sys::process::kill_group(pgid, nxb_sys::process::Signal::Kill);
        }
    }

    /// True after [`ProcessRegistry::kill_all`].
    pub fn is_cancelled(&self) -> bool {
        self.lock().cancelled
    }

    /// Number of live groups.
    pub fn live(&self) -> usize {
        self.lock().live.len()
    }

    /// Add a group, false (and the group is killed) when the run is already cancelled.
    pub(crate) fn register(&self, pgid: u32) -> bool {
        let mut g = self.lock();
        if g.cancelled {
            let _ = nxb_sys::process::kill_group(pgid, nxb_sys::process::Signal::Kill);
            return false;
        }
        g.live.insert(pgid);
        ALL_LIVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(pgid);
        true
    }

    pub(crate) fn unregister(&self, pgid: u32) {
        self.lock().live.remove(&pgid);
        ALL_LIVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&pgid);
    }
}

/// Removes the group from the registry when the job ends, before the reap.
struct Registered<'a> {
    reg: &'a ProcessRegistry,
    pgid: u32,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.reg.unregister(self.pgid);
    }
}

/// Bytes kept from a pipe and whether more arrived than the cap.
type Captured = io::Result<(Vec<u8>, bool)>;

fn join_reader(h: Option<std::thread::ScopedJoinHandle<'_, Captured>>) -> Captured {
    match h {
        Some(h) => h
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("pipe reader panicked"))),
        None => Ok((Vec::new(), false)),
    }
}

fn read_capped(mut r: impl Read, cap: usize) -> Captured {
    let mut kept = Vec::new();
    let mut over = false;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => return Ok((kept, over)),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
        if n > room {
            over = true;
        }
    }
}

fn head(stderr: &[u8]) -> String {
    String::from_utf8_lossy(&stderr[..stderr.len().min(STDERR_HEAD)]).into_owned()
}

/// Run one invocation to completion. Non-zero exit, signal, timeout, cancellation and pipe errors are `ToolError`s.
/// a machine problem at spawn carries an `EnvError` in `ToolError::env`.
pub fn run_process(
    tool: &str,
    inv: &Invocation,
    reg: &ProcessRegistry,
) -> Result<Completed, ToolError> {
    imp::run(tool, inv, reg)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use nxb_sys::process::{Signal, kill_group, wait_exit_nowait};
    use std::io::Write;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Command, Stdio};
    use std::sync::mpsc::{self, RecvTimeoutError};

    #[allow(clippy::zombie_processes, reason = "reaped by nxb_sys::process::wait4")]
    pub(super) fn run(
        tool: &str,
        inv: &Invocation,
        reg: &ProcessRegistry,
    ) -> Result<Completed, ToolError> {
        let start = Instant::now();
        let mut cmd = Command::new(&inv.program);
        cmd.args(&inv.args).env_clear();
        for (k, v) in inv.env.vars() {
            cmd.env(k, v);
        }
        if let Some(d) = &inv.cwd {
            cmd.current_dir(d);
        }
        cmd.stdin(if inv.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

        let mut child = cmd.spawn().map_err(|e| {
            let ctx = inv.program.display().to_string();
            ToolError::new(tool, ToolFailure::Spawn(e.kind()))
                .with_stderr(e.to_string())
                .with_env(EnvError::classify(&e, IoOp::Spawn, &ctx))
        })?;
        let pid = child.id();
        let registered = Registered { reg, pgid: pid };
        reg.register(pid);
        if inv.nice > 0 {
            // a lower priority is a hint: a child that already exited or a refused call changes nothing about the run
            let _ = nxb_sys::process::set_group_nice(pid, inv.nice);
        }

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let input: &[u8] = inv.stdin.as_deref().unwrap_or(&[]);

        let (timed_out, out, err) = std::thread::scope(|s| {
            if let Some(mut pipe) = stdin {
                // A child that stops reading gets EPIPE here once it is gone, that is not our failure.
                s.spawn(move || {
                    let _ = pipe.write_all(input);
                });
            }
            // Each pipe reader reports on `done_tx` when it hits EOF.
            let (done_tx, done_rx) = mpsc::channel::<()>();
            let mut readers = 0usize;
            let mut spawn_reader = |pipe: Option<Box<dyn Read + Send>>, cap: usize| {
                pipe.map(|p| {
                    readers += 1;
                    let tx = done_tx.clone();
                    s.spawn(move || {
                        let r = read_capped(p, cap);
                        let _ = tx.send(());
                        r
                    })
                })
            };
            let out_h = spawn_reader(
                stdout.map(|p| Box::new(p) as Box<dyn Read + Send>),
                inv.max_stdout,
            );
            let err_h = spawn_reader(
                stderr.map(|p| Box::new(p) as Box<dyn Read + Send>),
                inv.max_stderr,
            );
            drop(done_tx);
            let (tx, rx) = mpsc::channel::<()>();
            s.spawn(move || {
                // Never report an exit that did not happen: an unexpected waitid error is retried, so the group
                // is only killed after the leader really exited (or at the deadline).
                while wait_exit_nowait(pid).is_err() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let _ = tx.send(());
            });
            // Wait for the leader's exit. After the deadline or a cancel, kill the group and repeat the kill until the
            // leader is gone: one killpg can miss a process that is forking at that instant.
            let deadline = start + inv.timeout;
            let mut timed_out = false;
            loop {
                let now = Instant::now();
                let killing = timed_out || reg.is_cancelled();
                let wait = if killing {
                    KILL_RECHECK
                } else {
                    deadline.saturating_duration_since(now).min(CANCEL_RECHECK)
                };
                match rx.recv_timeout(wait) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {
                        if Instant::now() >= deadline {
                            timed_out = true;
                        }
                        if timed_out || reg.is_cancelled() {
                            let _ = kill_group(pid, Signal::Kill);
                        }
                    }
                }
            }
            // The leader has exited and is not reaped: kill stragglers in its group so every pipe closes. A process
            // that was being forked during the kill can survive it, so repeat until every reader has seen EOF.
            let _ = kill_group(pid, Signal::Kill);
            let mut finished = 0usize;
            while finished < readers {
                match done_rx.recv_timeout(STRAGGLER_RECHECK) {
                    Ok(()) => finished += 1,
                    Err(RecvTimeoutError::Timeout) => {
                        let _ = kill_group(pid, Signal::Kill);
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            (timed_out, join_reader(out_h), join_reader(err_h))
        });

        drop(registered);
        // Reap with wait4 for the child's CPU time and peak RSS (scheduler history, PCE utilisation).
        let (raw, usage) = nxb_sys::process::wait4(pid).map_err(|e| {
            ToolError::new(tool, ToolFailure::Io(e.kind())).with_stderr(e.to_string())
        })?;
        let status = std::process::ExitStatus::from_raw(raw);
        // `child` is reaped already, dropping a std Child neither waits nor kills.
        drop(child);
        let wall = start.elapsed();

        let (stderr, _) = err.unwrap_or_default();
        let fail = |f: ToolFailure| ToolError::new(tool, f).with_stderr(head(&stderr));
        if timed_out {
            return Err(fail(ToolFailure::Timeout(inv.timeout)));
        }
        // A job of a cancelled run never counts as a success, whatever status its leader reported (a shell may exit 0
        // after its children were killed).
        if reg.is_cancelled() {
            return Err(fail(ToolFailure::Cancelled));
        }
        if let Some(sig) = status.signal() {
            return Err(fail(ToolFailure::Signal(sig)));
        }
        match status.code() {
            Some(0) => {}
            Some(c) => return Err(fail(ToolFailure::Exit(c))),
            None => return Err(fail(ToolFailure::Signal(0))),
        }
        let (stdout, over) = out.map_err(|e| fail(ToolFailure::Io(e.kind())))?;
        if over {
            return Err(fail(ToolFailure::Io(io::ErrorKind::FileTooLarge)));
        }
        Ok(Completed {
            stdout,
            stderr,
            wall,
            usage,
        })
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    // Windows needs Job Objects for the group kill (design-v2 9.4), until nxb-sys has them, refuse instead of leaking.
    pub(super) fn run(
        tool: &str,
        _inv: &Invocation,
        _reg: &ProcessRegistry,
    ) -> Result<Completed, ToolError> {
        Err(ToolError::new(
            tool,
            ToolFailure::Spawn(io::ErrorKind::Unsupported),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::env::EnvPolicy;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    /// "Much less than the 30 s sleeps": generous because the endpoint scanner on the build Mac can stall an exec by
    /// seconds under load (seen: 12.8 s at load average 50).
    const QUICK: Duration = Duration::from_secs(25);

    fn sh(script: &str) -> Invocation {
        Invocation::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env(
                EnvPolicy::empty()
                    .set("PATH", "/usr/bin:/bin")
                    .unwrap_or_default(),
            )
            .timeout(Duration::from_secs(20))
    }

    #[test]
    fn exact_environment() {
        let env =
            EnvPolicy::apple(Path::new("/sdk/X"), Path::new("/tmp"), Path::new("/h")).unwrap();
        let inv = Invocation::new("/usr/bin/env").env(env);
        let out = run_process("env", &inv, &ProcessRegistry::new()).unwrap();
        let mut lines: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                "HOME=/h",
                "PATH=/usr/bin:/bin",
                "SDKROOT=/sdk/X",
                "TMPDIR=/tmp"
            ]
        );
    }

    #[test]
    fn stdin_round_trip_large() {
        // 4 MiB through cat: both pipes far beyond their buffers, no deadlock.
        let data: Vec<u8> = (0..4 << 20).map(|i: u32| (i * 31 % 251) as u8).collect();
        let inv = Invocation::new("/bin/cat")
            .stdin(data.clone())
            .timeout(Duration::from_secs(20));
        let out = run_process("cat", &inv, &ProcessRegistry::new()).unwrap();
        assert_eq!(out.stdout, data);
    }

    #[test]
    fn nonzero_exit_keeps_stderr() {
        let e =
            run_process("t", &sh("echo oops >&2; exit 3"), &ProcessRegistry::new()).unwrap_err();
        assert_eq!(e.failure, ToolFailure::Exit(3));
        assert_eq!(e.stderr_head, "oops\n");
        assert!(e.env.is_none());
    }

    #[test]
    fn crash_is_signal() {
        let e = run_process("t", &sh("kill -SEGV $$"), &ProcessRegistry::new()).unwrap_err();
        assert_eq!(e.failure, ToolFailure::Signal(11));
    }

    #[test]
    fn timeout_kills_group_quickly() {
        let t = Instant::now();
        let inv = sh("/bin/sleep 30 & /bin/sleep 30; wait").timeout(Duration::from_millis(300));
        let e = run_process("t", &inv, &ProcessRegistry::new()).unwrap_err();
        assert!(matches!(e.failure, ToolFailure::Timeout(_)));
        assert!(t.elapsed() < QUICK, "{:?}", t.elapsed());
    }

    #[test]
    fn grandchild_holding_stdout_does_not_hang() {
        // The leader exits at once, a grandchild keeps stdout open for 30 s. The straggler kill closes it.
        let t = Instant::now();
        let out = run_process(
            "t",
            &sh("(/bin/sleep 30; echo late) & echo started"),
            &ProcessRegistry::new(),
        )
        .unwrap();
        // A straggler caught mid-fork by the first kill may still print before the re-kill: only the prefix is ours.
        assert!(out.stdout.starts_with(b"started\n"), "{:?}", out.stdout);
        assert!(t.elapsed() < QUICK, "{:?}", t.elapsed());
    }

    #[test]
    fn stdout_limit_is_an_error() {
        let mut inv = sh("head -c 100000 /dev/zero");
        inv.max_stdout = 1000;
        let e = run_process("t", &inv, &ProcessRegistry::new()).unwrap_err();
        assert_eq!(e.failure, ToolFailure::Io(io::ErrorKind::FileTooLarge));
    }

    #[test]
    fn stderr_is_capped_not_failed() {
        let mut inv = sh("head -c 100000 /dev/zero >&2");
        inv.max_stderr = 10;
        let out = run_process("t", &inv, &ProcessRegistry::new()).unwrap();
        assert_eq!(out.stderr.len(), 10);
    }

    #[test]
    fn missing_program_is_env_error() {
        let inv = Invocation::new("/nonexistent/metal");
        let e = run_process("metal", &inv, &ProcessRegistry::new()).unwrap_err();
        assert_eq!(e.failure, ToolFailure::Spawn(io::ErrorKind::NotFound));
        assert_eq!(
            e.env.map(|x| x.kind),
            Some(nxb_error::EnvKind::ToolchainMissing)
        );
    }

    #[test]
    fn kill_all_cancels_running_and_later_jobs() {
        let reg = Arc::new(ProcessRegistry::new());
        let r2 = reg.clone();
        let h = std::thread::spawn(move || run_process("t", &sh("/bin/sleep 30"), &r2));
        let t = Instant::now();
        while reg.live() == 0 && t.elapsed() < QUICK {
            std::thread::sleep(Duration::from_millis(5));
        }
        reg.kill_all();
        let e = h.join().unwrap().unwrap_err();
        assert_eq!(e.failure, ToolFailure::Cancelled);
        assert_eq!(reg.live(), 0);
        let e = run_process("t", &sh("/bin/sleep 30"), &reg).unwrap_err();
        assert_eq!(e.failure, ToolFailure::Cancelled);
        assert!(t.elapsed() < QUICK);
    }

    /// Property-style: many concurrent jobs with random exit codes all report exactly their own code and output.
    #[test]
    fn concurrent_jobs_isolated() {
        let reg = ProcessRegistry::new();
        std::thread::scope(|s| {
            for i in 0..16u32 {
                let reg = &reg;
                s.spawn(move || {
                    let code = i % 3;
                    let r = run_process("t", &sh(&format!("echo {i}; exit {code}")), reg);
                    match r {
                        Ok(c) => {
                            assert_eq!(code, 0);
                            assert_eq!(c.stdout, format!("{i}\n").into_bytes());
                        }
                        Err(e) => assert_eq!(e.failure, ToolFailure::Exit(code as i32)),
                    }
                });
            }
        });
        assert_eq!(reg.live(), 0);
    }
}
