//! The pipeline archive in shards: one `MTL4Archive` for each build batch, opened when the first pipeline of the shard is
//! asked for, and at most a few open at once.
//!
//! `newArchiveWithURL` reads the whole file into memory, which is 4.8 GB for a big title. The build writes the batch
//! archives as separate files and an index (`nxkit_abi::shards`) from the identity of a pipeline (the stems of the stage
//! libraries and the function names) to the file. A lookup goes to the index first: a pipeline that is not in it is not
//! in any shard and is compiled, a pipeline that is gets its shard opened (from a least recently used set of
//! [`DEFAULT_OPEN`]) and asked.
//!
//! [`ShardSet`] is generic over what opening a shard gives, so the routing and the eviction run in tests without a GPU.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_abi::shards::{Func, INDEX_FILE, ShardIndex};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSArray, NSURL};
use objc2_metal::{MTL4Archive, MTL4CompilerTaskOptions, MTLDevice};

use crate::device::{Context, ns};
use crate::memstats::{Kind, Tracked};

use super::RenderPipelineDesc;
use super::lru::Lru;

/// How many shards stay open when the configuration does not say.
pub(crate) const DEFAULT_OPEN: usize = 32;

/// The shard that was asked for: its archive, and its file in the memory counters for as long as any copy lives.
#[derive(Clone)]
pub(crate) struct Open {
    pub(crate) archive: Retained<ProtocolObject<dyn MTL4Archive>>,
    _tracked: Arc<Tracked>,
}

impl Open {
    /// The memory counter of the file, to keep it alive as long as something else than the cache holds the archive.
    pub(crate) fn tracked(&self) -> Arc<Tracked> {
        self._tracked.clone()
    }
}

/// Opens an archive file. The archive of a shard counts its file size in the memory counters.
pub(crate) fn open_archive(ctx: &Context, path: &Path) -> Result<Open, String> {
    let p = path.to_str().ok_or("the archive path is not UTF-8")?;
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    let archive = ctx
        .device()
        .newArchiveWithURL_error(&NSURL::fileURLWithPath(&ns(p)))
        .map_err(|e| e.localizedDescription().to_string())?;
    crate::memstats::shard_opened();
    Ok(Open {
        archive,
        _tracked: Arc::new(Tracked::new(Kind::Shard, size)),
    })
}

/// Opens the archives that earlier runs of the host wrote. One that fails is reported and left out.
pub(crate) fn open_runtime(ctx: &Context, paths: &[PathBuf]) -> Vec<Open> {
    paths
        .iter()
        .filter_map(|p| match open_archive(ctx, p) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("nxkit-metal: runtime archive {}: {e}", p.display());
                None
            }
        })
        .collect()
}

/// Opens the shard file at a path.
type Opener<A> = Box<dyn Fn(&Path) -> Result<A, String> + Send + Sync>;

struct State<A> {
    open: Lru<u32, A>,
    /// Shards that failed to open. Each one is tried once: a missing file stays missing.
    failed: HashSet<u32>,
}

/// The shards of an app and the ones that are open.
pub(crate) struct ShardSet<A> {
    dir: PathBuf,
    index: ShardIndex,
    opener: Opener<A>,
    state: Mutex<State<A>>,
    opens: AtomicU64,
}

impl<A: Clone> ShardSet<A> {
    /// A set over the shards in `dir` that `index` describes. Nothing is opened yet.
    pub(crate) fn new(dir: PathBuf, index: ShardIndex, keep: usize, opener: Opener<A>) -> Self {
        Self {
            dir,
            index,
            opener,
            state: Mutex::new(State {
                open: Lru::new(if keep == 0 { DEFAULT_OPEN } else { keep }),
                failed: HashSet::new(),
            }),
            opens: AtomicU64::new(0),
        }
    }

    /// The shard that holds the render pipeline of these two functions, opened when it is not yet. None when no shard
    /// has the pair or the shard cannot be opened.
    pub(crate) fn render(&self, vertex: Func<'_>, fragment: Func<'_>) -> Option<A> {
        self.shard(self.index.render(vertex, fragment)?)
    }

    /// The shards that hold exact pipelines of the pair of these two functions (recorded states, fully specialized),
    /// each opened when it is not yet. Empty when the index has none for the pair or a shard cannot be opened.
    pub(crate) fn exact(&self, vertex: Func<'_>, fragment: Func<'_>) -> Vec<A> {
        self.index
            .exact(vertex, fragment)
            .iter()
            .filter_map(|n| self.shard(*n))
            .collect()
    }

    /// The number of pairs that have exact pipelines.
    pub(crate) fn exact_pairs(&self) -> usize {
        self.index.exact_pairs()
    }

    /// The shard that holds the compute pipeline of this function.
    pub(crate) fn compute(&self, function: Func<'_>) -> Option<A> {
        self.shard(self.index.compute(function)?)
    }

    fn shard(&self, n: u32) -> Option<A> {
        // The lock is held while a shard opens: two threads that want the same one open it once.
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(a) = st.open.get(&n) {
            return Some(a.clone());
        }
        if st.failed.contains(&n) {
            return None;
        }
        let path = self.dir.join(self.index.file(n)?);
        match (self.opener)(&path) {
            Ok(a) => {
                self.opens.fetch_add(1, Ordering::Relaxed);
                // what the insert pushes out is dropped here, and closed when no lookup still uses it
                drop(st.open.insert(n, a.clone()));
                Some(a)
            }
            Err(e) => {
                eprintln!("nxkit-metal: pipeline shard {}: {e}", path.display());
                st.failed.insert(n);
                None
            }
        }
    }

    /// The number of shard files.
    pub(crate) fn shards(&self) -> usize {
        self.index.shards()
    }

    /// The number of pipelines in the index.
    pub(crate) fn pipelines(&self) -> usize {
        self.index.pipelines()
    }

    /// The shards that are open now.
    pub(crate) fn open_now(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .open
            .len()
    }

    /// Closes every open shard and returns how many were open. A lookup that holds one keeps it until it is done, and
    /// a later lookup opens its shard again.
    pub(crate) fn close_all(&self) -> usize {
        let drained = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .open
            .drain();
        drained.len()
    }

    /// How many times a shard was opened (a closed one that is asked again counts again).
    pub(crate) fn opens(&self) -> u64 {
        self.opens.load(Ordering::Relaxed)
    }
}

impl ShardSet<Open> {
    /// The set of the shard folder `dir`: None when the folder has no index (an app of an older build). An index that
    /// cannot be read is reported and counts as none, so the single archive is still used.
    pub(crate) fn open(ctx: &Context, dir: &Path, keep: usize) -> Option<Self> {
        let bytes = match std::fs::read(dir.join(INDEX_FILE)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                eprintln!("nxkit-metal: shard index in {}: {e}", dir.display());
                return None;
            }
        };
        let index = match ShardIndex::decode(&bytes) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("nxkit-metal: shard index in {}: {e}", dir.display());
                return None;
            }
        };
        let ctx = ctx.clone();
        Some(Self::new(
            dir.to_path_buf(),
            index,
            keep,
            Box::new(move |p| open_archive(&ctx, p)),
        ))
    }

    /// The archive that holds the render pipeline of `desc`. A library that was not loaded from a file has no stem
    /// and is in no shard.
    pub(crate) fn render_archive(
        &self,
        desc: &RenderPipelineDesc<'_>,
    ) -> Option<Retained<ProtocolObject<dyn MTL4Archive>>> {
        let (v, f) = (desc.vertex, desc.fragment?);
        let found = self.render((v.0.stem()?, v.1), (f.0.stem()?, f.1))?;
        Some(found.archive)
    }

    /// The archives that hold the exact pipelines of the pair of `desc`. A library that was not loaded from a file has
    /// no stem and is in no shard.
    pub(crate) fn exact_archives(
        &self,
        desc: &RenderPipelineDesc<'_>,
    ) -> Vec<Retained<ProtocolObject<dyn MTL4Archive>>> {
        let Some((fl, fe)) = desc.fragment else {
            return Vec::new();
        };
        let (vl, ve) = desc.vertex;
        let (Some(vs), Some(fs)) = (vl.stem(), fl.stem()) else {
            return Vec::new();
        };
        self.exact((vs, ve), (fs, fe))
            .into_iter()
            .map(|o| o.archive)
            .collect()
    }

    /// The archive that holds the compute pipeline of `function` of `lib`.
    pub(crate) fn compute_archive(
        &self,
        lib: &super::ShaderLibrary,
        function: &str,
    ) -> Option<Retained<ProtocolObject<dyn MTL4Archive>>> {
        Some(self.compute((lib.stem()?, function))?.archive)
    }

    /// The task options of a compute compile: they look the pipeline up in its shard, when it has one.
    pub(crate) fn compute_options(
        &self,
        lib: &super::ShaderLibrary,
        function: &str,
    ) -> Option<Retained<MTL4CompilerTaskOptions>> {
        let found = self.compute((lib.stem()?, function))?;
        let o = MTL4CompilerTaskOptions::new();
        o.setLookupArchives(Some(&NSArray::from_retained_slice(std::slice::from_ref(
            &found.archive,
        ))));
        Some(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_abi::shards::IndexBuilder;
    use std::sync::atomic::AtomicUsize;

    /// An index of `n` shards `s<i>`, shard i holding the render pair (v<i>, f) and the compute function c<i>.
    fn set(
        n: u32,
        keep: usize,
        fail: &'static [&'static str],
    ) -> (ShardSet<String>, Arc<AtomicUsize>) {
        let mut b = IndexBuilder::new();
        for i in 0..n {
            let s = b.shard(&format!("s{i}"));
            b.render((&format!("v{i}_b"), "main0"), ("f_b", "main0"), s);
            b.compute((&format!("c{i}_b"), "kern"), s);
        }
        let index = ShardIndex::decode(&b.encode()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let opener = move |p: &Path| {
            c.fetch_add(1, Ordering::SeqCst);
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if fail.contains(&name.as_str()) {
                Err("no such file".to_string())
            } else {
                Ok(name)
            }
        };
        (
            ShardSet::new(PathBuf::from("/dir"), index, keep, Box::new(opener)),
            calls,
        )
    }

    #[test]
    fn nothing_opens_before_the_first_lookup() {
        let (s, calls) = set(5, 2, &[]);
        assert_eq!((s.shards(), s.pipelines(), s.open_now()), (5, 10, 0));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_pair_opens_its_own_shard_once() {
        let (s, calls) = set(5, 4, &[]);
        assert_eq!(
            s.render(("v3_b", "main0"), ("f_b", "main0")).as_deref(),
            Some("s3")
        );
        assert_eq!(
            s.render(("v3_b", "main0"), ("f_b", "main0")).as_deref(),
            Some("s3")
        );
        assert_eq!(s.compute(("c3_b", "kern")).as_deref(), Some("s3"));
        assert_eq!(
            (calls.load(Ordering::SeqCst), s.open_now(), s.opens()),
            (1, 1, 1)
        );
    }

    #[test]
    fn exact_pipelines_open_every_shard_of_their_pair_and_no_other() {
        let mut b = IndexBuilder::new();
        let base = b.shard("base");
        let (e1, e2) = (b.shard("e1"), b.shard("e2"));
        b.render(("v_b", "main0"), ("f_b", "main0"), base);
        b.exact(("v_b", "main0"), ("f_b", "main0"), e1);
        b.exact(("v_b", "main0"), ("f_b", "main0"), e2);
        let index = ShardIndex::decode(&b.encode()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let s = ShardSet::new(
            PathBuf::from("/dir"),
            index,
            4,
            Box::new(move |p: &Path| {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(p.file_name().unwrap().to_string_lossy().to_string())
            }),
        );
        assert_eq!(s.exact_pairs(), 1);
        // a pair without exact pipelines opens nothing
        assert!(s.exact(("v_b", "main0"), ("g_b", "main0")).is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(s.exact(("v_b", "main0"), ("f_b", "main0")), ["e1", "e2"]);
        // the base shard stays closed: asking for exact pipelines does not read it
        assert_eq!((calls.load(Ordering::SeqCst), s.open_now()), (2, 2));
        assert_eq!(
            s.render(("v_b", "main0"), ("f_b", "main0")).as_deref(),
            Some("base")
        );
    }

    #[test]
    fn a_pipeline_the_index_lacks_opens_nothing() {
        let (s, calls) = set(5, 4, &[]);
        assert!(s.render(("v9_b", "main0"), ("f_b", "main0")).is_none());
        assert!(s.render(("v1_b", "main0"), ("other_b", "main0")).is_none());
        assert!(s.compute(("v1_b", "main0")).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_least_recently_used_shard_closes_and_reopens_on_demand() {
        let (s, calls) = set(4, 2, &[]);
        let ask = |i: u32| s.compute((&format!("c{i}_b"), "kern"));
        for i in [0, 1] {
            assert!(ask(i).is_some());
        }
        // 0 is touched, so 1 is the one that closes when 2 opens
        assert!(ask(0).is_some());
        assert!(ask(2).is_some());
        assert_eq!((calls.load(Ordering::SeqCst), s.open_now()), (3, 2));
        assert!(ask(0).is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        // 1 was closed: asked again it opens again, and 2 goes
        assert_eq!(ask(1).as_deref(), Some("s1"));
        assert_eq!(
            (calls.load(Ordering::SeqCst), s.opens(), s.open_now()),
            (4, 4, 2)
        );
    }

    #[test]
    fn closing_all_shards_reopens_them_on_demand() {
        let (s, calls) = set(4, 4, &[]);
        let ask = |i: u32| s.compute((&format!("c{i}_b"), "kern"));
        for i in [0, 1, 2] {
            assert!(ask(i).is_some());
        }
        assert_eq!((s.close_all(), s.open_now()), (3, 0));
        assert_eq!(ask(1).as_deref(), Some("s1"));
        assert_eq!((calls.load(Ordering::SeqCst), s.open_now()), (4, 1));
    }

    #[test]
    fn a_shard_that_fails_to_open_is_tried_once() {
        let (s, calls) = set(3, 2, &["s1"]);
        assert!(s.compute(("c1_b", "kern")).is_none());
        assert!(s.compute(("c1_b", "kern")).is_none());
        assert!(s.render(("v1_b", "main0"), ("f_b", "main0")).is_none());
        assert_eq!((calls.load(Ordering::SeqCst), s.opens()), (1, 0));
        // the others still work
        assert!(s.compute(("c0_b", "kern")).is_some());
    }

    #[test]
    fn zero_keeps_the_default_number_open() {
        let (s, _) = set(40, 0, &[]);
        for i in 0..40 {
            assert!(s.compute((&format!("c{i}_b"), "kern")).is_some());
        }
        assert_eq!(s.open_now(), DEFAULT_OPEN);
    }

    #[test]
    fn threads_asking_for_one_shard_open_it_once() {
        let (s, calls) = set(2, 2, &[]);
        std::thread::scope(|t| {
            for _ in 0..8 {
                t.spawn(|| assert!(s.compute(("c1_b", "kern")).is_some()));
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
