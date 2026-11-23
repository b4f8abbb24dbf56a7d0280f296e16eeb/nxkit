//! The shader hot swap (W37): a person or an agent changes the MSL of a program in a running game and sees the effect
//! in the same scene.
//!
//! The host makes one [`ShaderHub`] and gives it to the GPU (`GpuSettings::shader_hub`) and to its control socket.
//! The control thread talks to the hub. The encode thread follows it (`Follower`, one atomic load per frame):
//!
//! - `list` counts the programs that the next frames draw with.
//! - `dump` copies the MSL of a program from `<run>/shader-src` to `<override>/<key>.metal`.
//! - `load` compiles `<override>/<key>.metal` with Metal and keeps the library. The follower puts it in place of the
//!   library of the program (`Programs::set_swaps`). The pipelines of the program have keys of their own, so they are
//!   built again. They compile on a compiler without a serializer and read no archive, so the runtime pipeline cache and
//!   the archive never get them (`PipelineCache::render_scratch`).
//! - `reset` takes the swap away. The shipped library and its cached pipelines are in force again.
//!
//! Without a swap, a draw pays two integer compares (`Last::swaps`). A frame pays one atomic load.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use nxkit_metal::{Context, ShaderLibrary};

use crate::detail::FrameDetail;
use crate::json::{Json, parse};

/// The frames that `list` counts, and how long it waits for them.
const LIST_FRAMES: u32 = 3;
const LIST_WAIT: Duration = Duration::from_secs(5);

/// The swapped libraries by program key, each with the number of its swap.
pub(crate) type SwapMap = BTreeMap<String, (ShaderLibrary, u32)>;

/// The swaps in force and the number of the next one.
#[derive(Default)]
pub(crate) struct Registry {
    swaps: SwapMap,
    next: u32,
}

impl Registry {
    /// Puts `lib` in place of the library of `key`. Gives the number of the swap (never 0, never used twice).
    pub(crate) fn put(&mut self, key: &str, lib: ShaderLibrary) -> u32 {
        self.next += 1;
        self.swaps.insert(key.to_owned(), (lib, self.next));
        self.next
    }

    /// Takes the swap of `key` away. False when there was none.
    pub(crate) fn remove(&mut self, key: &str) -> bool {
        self.swaps.remove(key).is_some()
    }

    /// Takes all swaps away. Gives the number of them.
    pub(crate) fn clear(&mut self) -> usize {
        let n = self.swaps.len();
        self.swaps.clear();
        n
    }
}

struct Attached {
    ctx: Context,
    shader_dir: PathBuf,
    /// The host draws vertex shaders through their `_bp` library (`NXKIT_PULL=1`).
    pull: bool,
}

#[derive(Default)]
struct State {
    attached: Option<Attached>,
    registry: Registry,
    /// The draws of the counted frames by program key and stage.
    counts: BTreeMap<(String, &'static str), u64>,
    frames: u32,
}

struct Inner {
    /// `<diag_dir>/shaders`: the sources that a person edits.
    dir: PathBuf,
    /// Changes with every swap and reset.
    epoch: AtomicU64,
    counting: AtomicBool,
    state: Mutex<State>,
    seen_frame: Condvar,
}

/// The meeting point of the control socket and the encoder. Clones share one state.
#[derive(Clone)]
pub struct ShaderHub(Arc<Inner>);

impl std::fmt::Debug for ShaderHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ShaderHub({})", self.0.dir.display())
    }
}

/// A key that names one file of the shader folder: no path.
fn valid_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.is_empty() || key.contains(['/', '\\', '.']) {
        return Err(format!("{key:?} is not a program key"));
    }
    Ok(key)
}

/// The names that the host looks up in the library of a program: the entry and the variants of the reflection, or the
/// pulling entry.
fn required(json: &Json, pulling: bool) -> Result<Vec<String>, String> {
    if pulling {
        return nxkit_pipestate::program::pull_entry(json)
            .map(|e| vec![e])
            .ok_or_else(|| "the reflection names no pulling entry".to_string());
    }
    let f = nxkit_pipestate::program::read(json, false)
        .map_err(|e| format!("the reflection does not read: {e:?}"))?;
    if f.mesh.is_some() {
        return Err("a task or mesh shader cannot be swapped yet".into());
    }
    let mut names = vec![f.entry];
    names.extend(f.entry_uint);
    names.extend(f.entry_mixed.map(|m| m.0));
    names.extend(f.entry_layer);
    names.extend(f.entry_z.into_iter().flatten());
    names.extend(f.entry_discard);
    Ok(names)
}

/// The names of `need` that `have` lacks.
fn missing(need: &[String], have: &[String]) -> Vec<String> {
    need.iter().filter(|n| !have.contains(n)).cloned().collect()
}

/// The lines of a Metal error that name an error, at most 8, on one line. The whole text goes to a file.
fn error_summary(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("error"))
        .take(8)
        .map(str::trim)
        .collect();
    if lines.is_empty() {
        text.lines().next().unwrap_or("").trim().to_owned()
    } else {
        lines.join(" | ")
    }
}

impl ShaderHub {
    /// A hub whose sources are in `dir` (`<diag_dir>/shaders`).
    pub fn new(dir: PathBuf) -> Self {
        Self(Arc::new(Inner {
            dir,
            epoch: AtomicU64::new(0),
            counting: AtomicBool::new(false),
            state: Mutex::new(State::default()),
            seen_frame: Condvar::new(),
        }))
    }

    /// The folder of the sources.
    pub fn dir(&self) -> &Path {
        &self.0.dir
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The GPU tells where the libraries are and which device compiles. Called once by `Gpu`.
    pub(crate) fn attach(&self, ctx: &Context, shader_dir: &Path, pull: bool) {
        self.state().attached = Some(Attached {
            ctx: ctx.clone(),
            shader_dir: shader_dir.to_owned(),
            pull,
        });
    }

    /// The shader folder and the device, or the reason there are none.
    fn attached(state: &State) -> Result<&Attached, String> {
        state
            .attached
            .as_ref()
            .ok_or_else(|| "the GPU has not started".to_string())
    }

    /// The reflection of `key` and whether the host draws it through its `_bp` library (a vertex shader with a `pull`
    /// field, with `NXKIT_PULL=1`). The library that the host uses is the one that a swap replaces.
    fn reflection(a: &Attached, key: &str) -> Result<(Json, bool), String> {
        let path = a.shader_dir.join(format!("{key}_b.json"));
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("no program {key}: {}: {e}", path.display()))?;
        let json = parse(&text).ok_or_else(|| format!("{} does not parse", path.display()))?;
        let pulling = a.pull && nxkit_pipestate::program::pull_entry(&json).is_some();
        Ok((json, pulling))
    }

    /// `shader list`: the programs that the next frames draw with.
    pub fn list(&self, filter: &str) -> Result<Vec<String>, String> {
        {
            let mut s = self.state();
            s.counts.clear();
            s.frames = 0;
        }
        self.0.counting.store(true, Ordering::Relaxed);
        let s = self.state();
        let (s, _) = self
            .0
            .seen_frame
            .wait_timeout_while(s, LIST_WAIT, |s| s.frames < LIST_FRAMES)
            .unwrap_or_else(PoisonError::into_inner);
        self.0.counting.store(false, Ordering::Relaxed);
        if s.frames == 0 {
            return Err(format!(
                "no frame in {} s: the game draws nothing now",
                LIST_WAIT.as_secs()
            ));
        }
        let frames = u64::from(s.frames);
        let mut rows: Vec<(&(String, &str), u64)> = s
            .counts
            .iter()
            .filter(|((k, _), _)| k.contains(filter))
            .map(|(k, n)| (k, n.div_ceil(frames)))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let mut out = vec![format!(
            "# {} frames, draws and dispatches per frame (* is swapped)",
            s.frames
        )];
        for ((key, stage), n) in rows {
            let star = if s.registry.swaps.contains_key(key) {
                " *"
            } else {
                ""
            };
            out.push(format!("{key} {stage} {n}{star}"));
        }
        Ok(out)
    }

    /// `shader dump KEY`: copies the shipped MSL of the program to `<override>/<key>.metal`.
    pub fn dump(&self, key: &str) -> Result<Vec<String>, String> {
        let key = valid_key(key)?;
        let s = self.state();
        let a = Self::attached(&s)?;
        let (_, pulling) = Self::reflection(a, key)?;
        let suffix = if pulling { "_bp" } else { "_b" };
        let from = a
            .shader_dir
            .with_file_name(nxkit_abi::shader_src::DIR)
            .join(nxkit_abi::shader_src::file_name(key, suffix));
        let src = std::fs::read_to_string(&from).map_err(|_| {
            format!(
                "no MSL for {key}: {} is missing. The app ships only the compiled libraries. Build the game again with {}=1 \
                 (nxb build keeps the MSL in {}), or translate the shader with the xlate-msl tool of nxb-xlatedev",
                from.display(),
                nxkit_abi::shader_src::KEEP_ENV,
                nxkit_abi::shader_src::DIR
            )
        })?;
        let to = self.0.dir.join(format!("{key}.metal"));
        if to.exists() {
            return Err(format!(
                "{} exists and keeps your edits. Remove it to dump the shipped source again",
                to.display()
            ));
        }
        std::fs::create_dir_all(&self.0.dir)
            .map_err(|e| format!("{}: {e}", self.0.dir.display()))?;
        std::fs::write(&to, &src).map_err(|e| format!("{}: {e}", to.display()))?;
        Ok(vec![format!(
            "wrote {} ({} lines, library {key}{suffix})",
            to.display(),
            src.lines().count()
        )])
    }

    /// `shader load KEY`: compiles `<override>/<key>.metal` and puts it in place of the program's library.
    pub fn load(&self, key: &str) -> Result<Vec<String>, String> {
        let key = valid_key(key)?;
        let path = self.0.dir.join(format!("{key}.metal"));
        let src = std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: {e} (shader dump {key} writes it)", path.display()))?;
        let (ctx, need) = {
            let s = self.state();
            let a = Self::attached(&s)?;
            let (json, pulling) = Self::reflection(a, key)?;
            (a.ctx.clone(), required(&json, pulling)?)
        };
        // The compile runs without the lock: it can take seconds.
        let lib = ShaderLibrary::from_source(&ctx, &format!("swap-{key}"), &src).map_err(|e| {
            let text = e.to_string();
            let errors = path.with_extension("errors.txt");
            let _ = std::fs::write(&errors, &text);
            format!(
                "{key}.metal does not compile (all of it is in {}): {}",
                errors.display(),
                error_summary(&text)
            )
        })?;
        let lacks = missing(&need, &lib.function_names());
        if !lacks.is_empty() {
            return Err(format!(
                "{key}.metal lacks the functions {} that the host calls. Keep the names of the shipped source",
                lacks.join(", ")
            ));
        }
        let n = self.state().registry.put(key, lib);
        self.0.epoch.fetch_add(1, Ordering::Release);
        Ok(vec![format!(
            "{key}: swap {n} compiled. The next frame builds the pipelines of {key} again and draws with it"
        )])
    }

    /// `shader reset KEY|all`: back to the shipped function.
    pub fn reset(&self, what: &str) -> Result<Vec<String>, String> {
        let what = what.trim();
        let line = if what == "all" {
            format!("{} swaps removed", self.state().registry.clear())
        } else {
            let key = valid_key(what)?;
            if !self.state().registry.remove(key) {
                return Err(format!("{key} has no swap"));
            }
            format!("{key}: back to the shipped function")
        };
        self.0.epoch.fetch_add(1, Ordering::Release);
        Ok(vec![line])
    }

    /// The swaps in force, one line each.
    pub fn swapped(&self) -> Vec<String> {
        let s = self.state();
        s.registry
            .swaps
            .iter()
            .map(|(k, (_, n))| format!("{k} swap {n}"))
            .collect()
    }

    /// Adds the programs of a frame to the count of `list`.
    fn count(&self, detail: &FrameDetail) {
        let mut seen: BTreeMap<(&str, &'static str), u64> = BTreeMap::new();
        fn add<'a>(
            m: &mut BTreeMap<(&'a str, &'static str), u64>,
            k: &'a str,
            stage: &'static str,
        ) {
            *m.entry((k, stage)).or_default() += 1;
        }
        for d in &detail.draws {
            let p = &d.program;
            if !p.vertex.is_empty() {
                add(&mut seen, &p.vertex, "vs");
            }
            for (k, stage) in [
                (&p.fragment, "fs"),
                (&p.task, "ts"),
                (&p.mesh, "ms"),
                (&p.geometry, "gs"),
            ] {
                if let Some(k) = k {
                    add(&mut seen, k, stage);
                }
            }
        }
        for d in &detail.dispatches {
            add(&mut seen, &d.program, "cs");
        }
        let mut s = self.state();
        for ((k, stage), n) in seen {
            *s.counts.entry((k.to_owned(), stage)).or_default() += n;
        }
        s.frames += 1;
        self.0.seen_frame.notify_all();
    }
}

/// The encode thread's side: it applies what the hub changed, once per frame.
pub(crate) struct Follower {
    hub: ShaderHub,
    seen: u64,
}

impl Follower {
    pub(crate) fn new(hub: ShaderHub, ctx: &Context, shader_dir: &Path, pull: bool) -> Self {
        hub.attach(ctx, shader_dir, pull);
        Self { hub, seen: 0 }
    }

    /// Counts the frame when `list` asks. Gives the swaps in force when they changed since the last call: the caller
    /// gives them to the programs and drops what holds pipelines of the old ones.
    pub(crate) fn tick(&mut self, detail: &FrameDetail) -> Option<SwapMap> {
        let epoch = self.hub.0.epoch.load(Ordering::Acquire);
        let changed = (epoch != self.seen).then(|| {
            self.seen = epoch;
            self.hub.state().registry.swaps.clone()
        });
        if self.hub.0.counting.load(Ordering::Relaxed) {
            self.hub.count(detail);
        }
        changed
    }
}

#[cfg(test)]
mod tests;
