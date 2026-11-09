//! The runtime pipeline cache: what the host compiled in earlier runs, kept for the next one.
//!
//! The build ships unspecialized pipelines only, so a title compiles (or specializes) the pipelines of every colour
//! state it uses while it plays. The host keeps them. Every pipeline that Metal made is captured by the cache's
//! pipeline data set serializer and written to `rt-*.mtl4archive` files in a writable folder of the title, together
//! with `pipelines.seen` (see `record.rs`), the list of the descriptions. At the next start the archives are lookup
//! archives of every compile of the cache, and `prewarm.rs` makes each pipeline of the list from them before the first
//! frame: archive lookups, no compiles. Whatever is new compiles once and is written for the run after.
//!
//! The folder is `<root>/pipecache/id-<identity>/`. The identity is a hash of the app build (`GpuConfig::build_id`), the
//! host executable, the macOS build and the GPU: the keys and the binaries are only good for that combination, so the
//! folder of any other identity is removed at the start. `NXKIT_PIPELINE_CACHE=0` turns the whole cache off.
//!
//! Writes: `pipelines.seen` is written every `NXKIT_PIPELINE_CACHE_FLUSH` seconds (30) when the list grew; a new
//! archive file when pipelines were compiled since the last one, at most every 30 s at first and then at doubling
//! gaps up to 10 minutes (a file is another lookup archive, so a run should not make dozens), and both at the exit
//! ([`flush_runtime_cache`]). Every write goes to a temporary file and is renamed. The archives are bounded by
//! `NXKIT_PIPELINE_CACHE_MB` (512) and 64 files: the oldest go first, and what they held compiles again when asked.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nxkit_metal::{ArchiveConfig, Context, PipelineCache};

use super::Pipelines;
use super::record::{self, Seen};

/// The folder of the cache under the root the host chose.
const DIR: &str = "pipecache";
/// The list of the descriptions, in the folder of the identity.
pub(super) const SEEN_FILE: &str = "pipelines.seen";
const ARCHIVE_PREFIX: &str = "rt-";
const ARCHIVE_EXT: &str = "mtl4archive";
/// The most archive files the folder keeps.
const MAX_FILES: usize = 64;
/// The longest gap between two archive files of a run.
const MAX_GAP: Duration = Duration::from_secs(600);

/// `name` is set and not `0`.
pub(super) fn env_on(name: &str) -> bool {
    std::env::var(name).map_or(true, |v| v != "0")
}

pub(super) fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// A 64-bit FNV-1a hash of the parts, which are told apart.
pub(super) fn identity(parts: &[&str]) -> u64 {
    parts.iter().fold(0xcbf2_9ce4_8422_2325, |h, p| {
        p.bytes()
            .chain([0xff])
            .fold(h, |h, c| (h ^ u64::from(c)).wrapping_mul(0x100_0000_01b3))
    })
}

/// The build of macOS, from `SystemVersion.plist`.
fn os_build() -> String {
    std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
        .ok()
        .and_then(|t| {
            let rest = t.split("<key>ProductBuildVersion</key>").nth(1)?;
            let v = rest.split("<string>").nth(1)?.split("</string>").next()?;
            Some(v.trim().to_string())
        })
        .unwrap_or_default()
}

/// The size and the modification time of the running executable: another host build makes other keys.
fn exe_stamp() -> String {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{t}", m.len())
        })
        .unwrap_or_default()
}

/// The archive files of a folder with their sizes, oldest first (the names carry the time).
fn archives(dir: &Path) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let ok = name.starts_with(ARCHIVE_PREFIX) && name.ends_with(&format!(".{ARCHIVE_EXT}"));
            ok.then(|| (name, e.metadata().map_or(0, |m| m.len())))
        })
        .collect();
    v.sort();
    v
}

/// The files to remove so that the rest fits in `cap` bytes and `max` files: the oldest first. `files` is oldest first.
pub(super) fn prune(files: &[(String, u64)], cap: u64, max: usize) -> Vec<String> {
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let mut left = files.len();
    let mut out = Vec::new();
    for (name, size) in files {
        if total <= cap && left <= max {
            break;
        }
        total -= size;
        left -= 1;
        out.push(name.clone());
    }
    out
}

/// The name of the next archive file: the time in seconds and the number in the run.
fn archive_name(seq: u32) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("{ARCHIVE_PREFIX}{secs:012}-{seq:04}.{ARCHIVE_EXT}")
}

/// What [`plan`] found in the folder of the cache.
pub(crate) struct Plan {
    dir: PathBuf,
    /// The descriptions of the earlier runs.
    pub(super) seen: Seen,
    files: usize,
    bytes: u64,
}

/// Opens the runtime cache of the title under `root`: makes the folder of this identity, removes the others, keeps it
/// within its size, and adds the archives to `archive` as lookup archives. The cache then collects what it builds
/// (`archive.collect`). None when the cache is off or the folder cannot be made.
pub(crate) fn plan(
    ctx: &Context,
    root: Option<&Path>,
    build_id: u64,
    archive: &mut ArchiveConfig,
) -> Option<Plan> {
    let root = root.filter(|_| env_on("NXKIT_PIPELINE_CACHE"))?;
    let id = identity(&[
        "nxkit pipecache 1",
        &build_id.to_string(),
        &exe_stamp(),
        &os_build(),
        &ctx.name(),
    ]);
    let base = root.join(DIR);
    let own = format!("id-{id:016x}");
    let dir = base.join(&own);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("nxkit-gpu: pipeline cache {}: {e}", dir.display());
        return None;
    }
    // Another identity is another host, app build, macOS or GPU: nothing of it can be used.
    for e in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("id-") && name != own {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().ends_with(".partial") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let cap = env_num("NXKIT_PIPELINE_CACHE_MB", 512) << 20;
    let files = archives(&dir);
    for name in prune(&files, cap, MAX_FILES) {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let files = archives(&dir);
    archive.runtime = files.iter().map(|f| dir.join(&f.0)).collect();
    archive.collect = true;
    let seen = match std::fs::read_to_string(dir.join(SEEN_FILE)) {
        Ok(t) => Seen::parse(&t).unwrap_or_else(|e| {
            eprintln!("nxkit-gpu: {SEEN_FILE} is not used: {e}");
            Seen::default()
        }),
        Err(_) => Seen::default(),
    };
    if record::record_on() {
        record::arm(seen.clone());
    }
    Some(Plan {
        dir,
        seen,
        files: files.len(),
        bytes: files.iter().map(|f| f.1).sum(),
    })
}

/// What the periodic and the exit write share.
struct Session {
    dir: PathBuf,
    cache: Arc<PipelineCache>,
    cap: u64,
    seq: AtomicU32,
    io: Mutex<Io>,
}

struct Io {
    last_archive: Instant,
    gap: Duration,
}

static SESSION: Mutex<Option<Arc<Session>>> = Mutex::new(None);

/// Writes what the run has made so far, the list and the archive, for the next run. The host calls it at the exit; a
/// thread of the cache calls it every 30 s. Does nothing when the runtime cache is off.
pub fn flush_runtime_cache() {
    let s = SESSION
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(s) = s {
        s.flush(true);
    }
}

/// Writes a file through a temporary one that is renamed.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("partial");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Session {
    fn flush(&self, force: bool) {
        let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(mut mine) = record::take_dirty() {
            let path = self.dir.join(SEEN_FILE);
            // another run of the same title may have written entries since the start
            if let Some(old) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| Seen::parse(&t).ok())
            {
                mine.merge(&old);
            }
            if let Err(e) = write_atomic(&path, mine.to_text().as_bytes()) {
                record::mark_dirty();
                eprintln!("nxkit-gpu: {}: {e}", path.display());
            }
        }
        if !force && io.last_archive.elapsed() < io.gap {
            return;
        }
        let fresh = record::take_fresh();
        if fresh == 0 {
            return;
        }
        match self.write_archive() {
            Ok(()) => {
                io.last_archive = Instant::now();
                io.gap = (io.gap * 2).min(MAX_GAP);
            }
            Err(e) => {
                record::restore_fresh(fresh);
                eprintln!("nxkit-gpu: runtime pipeline archive: {e}");
            }
        }
    }

    fn write_archive(&self) -> Result<(), String> {
        let name = archive_name(self.seq.fetch_add(1, Ordering::Relaxed));
        let (to, tmp) = (
            self.dir.join(&name),
            self.dir.join(format!("{name}.partial")),
        );
        self.cache.save_archive(&tmp).map_err(|e| e.to_string())?;
        let size = std::fs::metadata(&tmp).map_err(|e| e.to_string())?.len();
        if size == 0 {
            let _ = std::fs::remove_file(&tmp);
            return Ok(());
        }
        std::fs::rename(&tmp, &to).map_err(|e| e.to_string())?;
        for old in prune(&archives(&self.dir), self.cap, MAX_FILES) {
            let _ = std::fs::remove_file(self.dir.join(old));
        }
        Ok(())
    }
}

impl Pipelines {
    /// Starts the runtime cache: makes the pipelines of the list from the archives (see `prewarm.rs`), and starts the
    /// thread that writes the cache every 30 s. Logs one line with the counts.
    pub(crate) fn runtime_start(&self, plan: Plan, shader_dir: &Path) {
        eprintln!(
            "nxkit-gpu: runtime pipeline cache {}: {} archives ({} MiB), {} pipelines listed",
            plan.dir.display(),
            plan.files,
            plan.bytes >> 20,
            plan.seen.len()
        );
        if env_on("NXKIT_PIPELINE_PREWARM") && !plan.seen.is_empty() {
            self.prewarm(&plan.seen, shader_dir);
        }
        let flush_s = env_num("NXKIT_PIPELINE_CACHE_FLUSH", 30);
        let session = Arc::new(Session {
            dir: plan.dir,
            cache: self.cache.clone(),
            cap: env_num("NXKIT_PIPELINE_CACHE_MB", 512) << 20,
            seq: AtomicU32::new(0),
            io: Mutex::new(Io {
                last_archive: Instant::now(),
                gap: Duration::from_secs(flush_s),
            }),
        });
        *SESSION.lock().unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
        let spawned = std::thread::Builder::new()
            .name("nxkit-pipecache".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(flush_s));
                    session.flush(false);
                }
            });
        if let Err(e) = spawned {
            eprintln!("nxkit-gpu: the pipeline cache thread did not start: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
