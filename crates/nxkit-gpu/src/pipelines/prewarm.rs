//! The pipelines of the list, made before the first frame.
//!
//! The list (`record.rs`) says which pipelines the earlier runs needed. This module makes each one under the key that
//! the encoder will ask for, so the cache holds it when the first draw comes. A pipeline is made through the same
//! calls as a draw's (`build_render`, `mesh_traced`, `compute`), and the runtime archives are lookup archives of the
//! compiler, so it is an archive lookup and not a compile; one that the archives lack compiles here, once.
//!
//! The start waits for the work, up to `NXKIT_PIPELINE_PREWARM_WAIT` seconds (20): a draw that finds its pipeline
//! missing waits a frame or more for a background compile, and a start that is a few seconds longer costs less than the
//! hitches of the first minutes. Waiting is bounded because the guest's first call into the GPU must not stall for a
//! long time on a list that grew over many sessions: what is not done by then goes on in the background threads while
//! the first frames run (a draw that asks for a pipeline the threads have not reached builds it itself, as before). The
//! work runs on several threads, as many as the background builds use. One line gives the counts and the time, and a
//! line every 2 s says how far it is while the start waits.
//!
//! When the list is done the open shards of the build archive are closed (`NXKIT_SHARD_TRIM=0` keeps them).
//! `newArchiveWithURL` holds each shard file in malloc memory (1.1 to 1.6 GB measured for 34 to 38 open archives), and
//! idle after the prewarm it is the first memory macOS compresses; a later draw that needs a shard opens it again.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Condvar;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

use super::record::{Entry, MeshRec, RenderRec, Seen};
use super::*;

/// What became of one entry.
#[derive(Clone, Copy)]
enum Outcome {
    Cached,
    Archive,
    Specialized,
    Compiled,
    Compute,
    Failed,
    Skipped,
}

const NAMES: [&str; 7] = [
    "cached",
    "archive",
    "specialized",
    "compiled",
    "compute",
    "failed",
    "skipped",
];

impl From<Source> for Outcome {
    fn from(s: Source) -> Self {
        match s {
            Source::Cached => Self::Cached,
            Source::Archive(_) => Self::Archive,
            Source::Specialized(_) => Self::Specialized,
            Source::Compiled(_) => Self::Compiled,
        }
    }
}

/// The shared state of the worker threads.
struct Work {
    entries: Vec<Entry>,
    next: AtomicUsize,
    finished: AtomicUsize,
    alive: AtomicUsize,
    counts: [AtomicUsize; 7],
    libs: Mutex<HashMap<String, Option<ShaderLibrary>>>,
    cache: Arc<PipelineCache>,
    fallbacks: Arc<Mutex<FastSet<u64>>>,
    ctx: Context,
    dir: PathBuf,
    start: Instant,
    /// The start stopped waiting: the last worker prints the line.
    detached: AtomicBool,
    over: (Mutex<()>, Condvar),
}

impl Work {
    fn lib(&self, stem: &str) -> Option<ShaderLibrary> {
        if let Some(l) = self
            .libs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(stem)
        {
            return l.clone();
        }
        let path = self.dir.join(format!("{stem}.metallib"));
        let l = path
            .is_file()
            .then(|| ShaderLibrary::from_path(&self.ctx, &path).ok())
            .flatten();
        self.libs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(stem.to_string(), l.clone());
        l
    }

    fn render(&self, r: &RenderRec) -> Option<Outcome> {
        let vl = self.lib(&r.vertex.0)?;
        let fl = match &r.fragment {
            Some((s, _)) => Some(self.lib(s)?),
            None => None,
        };
        let fragment = fl
            .as_ref()
            .zip(r.fragment.as_ref())
            .map(|(l, (_, e))| (l, e.as_str()));
        let mut desc = RenderPipelineDesc::new((&vl, r.vertex.1.as_str()), fragment, &[]);
        desc.colors = r.colors.clone();
        desc.vertex_layout = r.layout.clone();
        desc.topology = r.topology;
        desc.sample_count = r.samples;
        desc.alpha_to_coverage = r.a2c;
        desc.support_icb = r.icb;
        let at = if r.fallback {
            r.key ^ FALLBACK_KEY
        } else {
            r.key
        };
        if self.cache.cached_render(at).is_some() {
            return Some(Outcome::Cached);
        }
        if r.fallback {
            let mut set = self
                .fallbacks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            set.insert(r.key);
            set.insert(r.vertex_key);
        }
        Some(
            build_render(&self.cache, &self.fallbacks, r.key, r.vertex_key, &mut desc)
                .map_or(Outcome::Failed, |(_, s)| s.into()),
        )
    }

    fn mesh(&self, m: &MeshRec) -> Option<Outcome> {
        let load = |s: &Option<(String, String)>| match s {
            Some((stem, e)) => self.lib(stem).map(|l| Some((l, e.clone()))),
            None => Some(None),
        };
        let (object, fragment) = (load(&m.object)?, load(&m.fragment)?);
        let mesh = (self.lib(&m.mesh.0)?, m.mesh.1.clone());
        if self.cache.cached_render(m.key).is_some() {
            return Some(Outcome::Cached);
        }
        fn as_ref(s: &Option<(ShaderLibrary, String)>) -> Option<(&ShaderLibrary, &str)> {
            s.as_ref().map(|(l, e)| (l, e.as_str()))
        }
        let desc = MeshPipelineDesc {
            object: as_ref(&object),
            mesh: (&mesh.0, mesh.1.as_str()),
            fragment: as_ref(&fragment),
            colors: m.colors.clone(),
            sample_count: m.samples,
            alpha_to_coverage: m.a2c,
            object_threads: m.object_threads,
            mesh_threads: m.mesh_threads,
            payload_bytes: m.payload,
        };
        Some(
            self.cache
                .mesh_traced(m.key, &desc)
                .map_or(Outcome::Failed, |(_, s)| s.into()),
        )
    }

    fn make(&self, e: &Entry) -> Outcome {
        let done = match e {
            Entry::Render(r) => self.render(r),
            Entry::Mesh(m) => self.mesh(m),
            Entry::Compute(c) => self.lib(&c.function.0).map(|l| {
                self.cache
                    .compute(c.key, &l, &c.function.1)
                    .map_or(Outcome::Failed, |_| Outcome::Compute)
            }),
        };
        done.unwrap_or(Outcome::Skipped)
    }

    fn line(&self, what: &str) -> String {
        let list: Vec<String> = NAMES
            .iter()
            .enumerate()
            .filter(|(i, _)| self.counts[*i].load(Ordering::Relaxed) > 0)
            .map(|(i, name)| format!("{name} {}", self.counts[i].load(Ordering::Relaxed)))
            .collect();
        format!(
            "nxkit-gpu: pipeline prewarm {what}: {} of {} in {:.1} s ({})",
            self.finished.load(Ordering::Relaxed),
            self.entries.len(),
            self.start.elapsed().as_secs_f64(),
            list.join(", ")
        )
    }
}

fn worker(w: &Work) {
    nxkit_metal::thread::set_user_initiated();
    loop {
        let i = w.next.fetch_add(1, Ordering::Relaxed);
        let Some(e) = w.entries.get(i) else { break };
        // a panic in one entry must not end the thread: the rest of the list still needs it
        let o = catch_unwind(AssertUnwindSafe(|| w.make(e))).unwrap_or(Outcome::Failed);
        w.counts[o as usize].fetch_add(1, Ordering::Relaxed);
        w.finished.fetch_add(1, Ordering::Relaxed);
    }
    if w.alive.fetch_sub(1, Ordering::AcqRel) == 1 {
        w.libs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        if w.detached.load(Ordering::Acquire) {
            eprintln!("{}", w.line("finished in the background"));
        }
        // the shards of the build archive sit idle in malloc memory from here on (see the module)
        if super::runtime::env_on("NXKIT_SHARD_TRIM") {
            let closed = w.cache.close_shards();
            if closed > 0 {
                eprintln!("nxkit-gpu: prewarm over, {closed} archive shards closed");
            }
        }
        let _g = w.over.0.lock().unwrap_or_else(PoisonError::into_inner);
        w.over.1.notify_all();
    }
}

fn threads() -> usize {
    std::env::var("NXKIT_PIPELINE_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 8))
        })
}

impl Pipelines {
    /// Makes the pipelines of `seen` from the libraries in `shader_dir`. Waits for them up to the budget (see the
    /// module); what is left goes on in the background.
    pub(super) fn prewarm(&self, seen: &Seen, shader_dir: &Path) {
        if seen.is_empty() {
            return;
        }
        let n = threads().min(seen.len());
        let w = Arc::new(Work {
            entries: seen.entries().to_vec(),
            next: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            alive: AtomicUsize::new(n),
            counts: Default::default(),
            libs: Mutex::new(HashMap::new()),
            cache: self.cache.clone(),
            fallbacks: self.topology_fallbacks.clone(),
            ctx: self.ctx.clone(),
            dir: shader_dir.to_path_buf(),
            start: Instant::now(),
            detached: AtomicBool::new(false),
            over: (Mutex::new(()), Condvar::new()),
        });
        eprintln!(
            "nxkit-gpu: compiling shaders: making {} pipelines on {n} threads",
            seen.len()
        );
        let mut started = 0;
        for i in 0..n {
            let me = w.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("nxkit-prewarm-{i}"))
                .spawn(move || worker(&me));
            started += usize::from(spawned.is_ok());
        }
        // a thread that did not start leaves its share to the others; none at all: the first draws build as before
        if started < n {
            w.alive.fetch_sub(n - started, Ordering::AcqRel);
        }
        let budget =
            Duration::from_secs(super::runtime::env_num("NXKIT_PIPELINE_PREWARM_WAIT", 20));
        let mut shown = Instant::now();
        let mut g = w.over.0.lock().unwrap_or_else(PoisonError::into_inner);
        while w.alive.load(Ordering::Acquire) > 0 && w.start.elapsed() < budget {
            g = w
                .over
                .1
                .wait_timeout(g, Duration::from_millis(250))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            if shown.elapsed() >= Duration::from_secs(2) {
                shown = Instant::now();
                eprintln!("{}", w.line("progress"));
            }
        }
        drop(g);
        if w.alive.load(Ordering::Acquire) == 0 {
            eprintln!("{}", w.line("done"));
        } else {
            w.detached.store(true, Ordering::Release);
            // the last worker may have ended between the check and the flag
            if w.alive.load(Ordering::Acquire) == 0 {
                eprintln!("{}", w.line("done"));
            } else {
                eprintln!("{}", w.line("continues in the background"));
            }
        }
    }
}
