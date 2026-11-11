//! The runtime pipeline cache: what the host compiled in earlier runs, kept for the next one.
//!
//! The build ships the pipelines of the states that a title recorded fully specialized, and unspecialized pipelines
//! for the rest (`nxb-archive`), so a title specializes or compiles the pipelines of a colour state that it had not
//! recorded while it plays. The host keeps them. Every pipeline that Metal **compiled** is captured by the cache's
//! pipeline data set serializer (a specialization is not: the capture of 59 specializations and one compile was 43 KB,
//! of 60 compiles 3.6 MB) and written to `rt-*.mtl4archive` files in a writable folder of the title, together with
//! `pipelines.seen` (see `record.rs`), the list of the descriptions. At the next start the archives are lookup
//! archives of the cache, and `prewarm.rs` makes each pipeline of the list from them before the first frame: archive
//! lookups, no compiles. A pipeline that no archive has is compiled by that start (not specialized, so that it is
//! captured), and the archive is written when the start's last worker is done, before the start reports and before
//! `prewarm.done`, so the run after loads it.
//!
//! The archives are in `<root>/pipecache/id-<identity>/`; the list is not: it is kept in
//! `<root>/pipecache/seen-<scheme>.list` across builds (`carry.rs`), so a rebuilt app starts with the list of the
//! earlier builds and makes its pipelines before the first frame. The identity is a hash of the app build (`GpuConfig::build_id`), the
//! key scheme ([`KEY_SCHEME`], see `keys.rs`), the macOS build and the GPU: the binaries are only good for that app
//! build, macOS and GPU, and the keys of the list only for that scheme, so the folder of any other identity is removed
//! at the start. The host executable is not part of it: a new host build with the same key scheme keeps the cache. `NXKIT_PIPELINE_CACHE=0` turns the whole cache off.
//!
//! Writes: the list (`seen-<scheme>.list`) is written every `NXKIT_PIPELINE_CACHE_FLUSH` seconds (30) when the list grew; a new
//! archive file when pipelines were compiled since the last one, at most every 30 s at first and then at doubling
//! gaps up to 10 minutes, and at once when the serializer has captured `NXKIT_PIPELINE_CACHE_ROTATE` (1000) pipelines (a file is another lookup archive, so a run should not make dozens), and both at the exit
//! ([`flush_runtime_cache`]). Every write goes to a temporary file and is renamed. The archives are bounded by
//! `NXKIT_PIPELINE_CACHE_MB` (512) and 64 files: the oldest go first, and what they held compiles again when asked.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nxkit_metal::{ArchiveConfig, Context, PipelineCache};

use super::KEY_SCHEME;
use super::Pipelines;
use super::carry;
use super::record::{self, Seen};

/// The folder of the cache under the root the host chose.
const DIR: &str = "pipecache";
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
    /// The carried list (`carry.rs`).
    list: PathBuf,
    /// The descriptions of the earlier runs and builds.
    pub(super) seen: Seen,
    /// The start has to make every pipeline of the list and wait for all of them: this identity has not done it yet.
    pub(super) full: bool,
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
        "nxkit pipecache 2",
        &build_id.to_string(),
        &KEY_SCHEME.to_string(),
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
    // An identity is an app build, a key scheme, a macOS build and a GPU. The archives hold binaries of that app
    // build for that macOS and GPU: nothing of another identity can be used. The list does not hold binaries and
    // is kept (`carry.rs`). Another host build with the same key scheme is the same identity.
    let carried = carry::load(&base, KEY_SCHEME);
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
    // The list lives outside the identity folders. The lists of the folders that were removed above are merged in
    // first (the folders of the old layout); they are read before the removal.
    let list = carry::list_path(&base, KEY_SCHEME);
    let seen = carried.seen;
    if record::record_on() {
        record::arm(seen.clone());
        if carried.migrated > 0 {
            record::mark_dirty();
        }
    }
    let full = carry::needs_full_start(&dir, &seen);
    Some(Plan {
        dir,
        list,
        seen,
        full,
        files: files.len(),
        bytes: files.iter().map(|f| f.1).sum(),
    })
}

/// What the periodic and the exit write share.
struct Session {
    dir: PathBuf,
    list: PathBuf,
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
        s.flush(true, false);
    }
}

/// Writes a file through a temporary one that is renamed.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("partial");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Session {
    /// Writes the list and, when it is time or `force` or `full`, the archive. `full`: the serializer has captured as
    /// many pipelines as a rotation allows, so it is handed over now whatever the gap.
    fn flush(&self, force: bool, full: bool) {
        let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(mut mine) = record::take_dirty() {
            let path = &self.list;
            // another run of the same title may have written entries since the start
            if let Some(old) = std::fs::read_to_string(path)
                .ok()
                .and_then(|t| Seen::parse(&t).ok())
            {
                mine.merge(&old);
            }
            if let Err(e) = write_atomic(path, carry::text(&mine, KEY_SCHEME).as_bytes()) {
                record::mark_dirty();
                eprintln!("nxkit-gpu: {}: {e}", path.display());
            }
        }
        if !force && !full && io.last_archive.elapsed() < io.gap {
            return;
        }
        let fresh = record::take_fresh();
        if fresh == 0 && !full {
            return;
        }
        match self.write_archive() {
            // a rotation with nothing captured has no file to write
            Err(_) if fresh == 0 => {
                io.last_archive = Instant::now();
            }
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
        // The serializer's store is limited, so every write hands the capture to a new serializer.
        self.cache.rotate_archive(&tmp).map_err(|e| e.to_string())?;
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
            "nxkit-gpu: runtime pipeline cache {}: {} archives ({} MiB), {} pipelines listed{}",
            plan.dir.display(),
            plan.files,
            plan.bytes >> 20,
            plan.seen.len(),
            if plan.full {
                " (this build has not made them yet)"
            } else {
                ""
            }
        );
        let flush_s = env_num("NXKIT_PIPELINE_CACHE_FLUSH", 30);
        let rotate_at = env_num("NXKIT_PIPELINE_CACHE_ROTATE", 1000);
        let marker = plan.full.then(|| plan.dir.join(carry::DONE_FILE));
        let session = Arc::new(Session {
            dir: plan.dir,
            list: plan.list,
            cache: self.cache.clone(),
            cap: env_num("NXKIT_PIPELINE_CACHE_MB", 512) << 20,
            seq: AtomicU32::new(0),
            io: Mutex::new(Io {
                last_archive: Instant::now(),
                gap: Duration::from_secs(flush_s),
            }),
        });
        *SESSION.lock().unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
        if env_on("NXKIT_PIPELINE_PREWARM") && !plan.seen.is_empty() {
            // The start compiles what no archive holds, and the runtime archive of it is written when the last
            // pipeline of the list is made, before the start reports done and before the marker: a start that is
            // killed right after has what it made on disk.
            let s = session.clone();
            self.prewarm(
                &plan.seen,
                shader_dir,
                marker,
                Some(Box::new(move || s.flush(true, false))),
            );
        }
        let spawned = std::thread::Builder::new()
            .name("nxkit-pipecache".into())
            .spawn(move || {
                let mut last = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    let full = session.cache.captured() >= rotate_at;
                    if full || last.elapsed() >= Duration::from_secs(flush_s) {
                        last = Instant::now();
                        session.flush(false, full);
                    }
                }
            });
        if let Err(e) = spawned {
            eprintln!("nxkit-gpu: the pipeline cache thread did not start: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
