//! The pipeline cache: pipeline states under a caller-chosen hash, and the archive that keeps them between runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSArray, NSURL};
use objc2_metal::{
    MTL4AlphaToCoverageState, MTL4Archive, MTL4Compiler, MTL4CompilerDescriptor,
    MTL4CompilerTaskOptions, MTL4ComputePipelineDescriptor, MTL4IndirectCommandBufferSupportState,
    MTL4LibraryDescriptor, MTL4MeshRenderPipelineDescriptor, MTL4PipelineDataSetSerializer,
    MTL4PipelineDataSetSerializerConfiguration, MTL4PipelineDataSetSerializerDescriptor,
    MTL4RenderPipelineDescriptor, MTLDevice,
};

use crate::device::{Context, RawCompiler, ns};
use crate::error::{MetalError, Result, create};
use crate::state::check_sample_count;

use super::shards::{self, ShardSet};
use super::{ComputePipeline, MeshPipelineDesc, RenderPipeline, RenderPipelineDesc, ShaderLibrary};
pub use specialize::BaseStats;
use specialize::Bases;

mod miss;
mod specialize;
mod stats;

/// The empty fragment function of a depth-only mesh pipeline.
const NOP_FRAGMENT: &str = "fragment void nxkit_mesh_nop() {}";
const NOP_FRAGMENT_NAME: &str = "nxkit_mesh_nop";

/// Where a pipeline cache reads and writes its archive.
#[derive(Clone, Debug, Default)]
pub struct ArchiveConfig {
    /// An archive to look pipelines up in before compiling. A missing file is not an error. Metal reads the whole file
    /// into memory, so a large archive belongs in `shards`. Not used when `shards` has an index.
    pub load: Option<PathBuf>,
    /// The folder of the archive shards and their index (`nxkit_abi::shards`). A shard is opened when the first
    /// pipeline in it is asked for. A folder without an index counts as none, and `load` is used.
    pub shards: Option<PathBuf>,
    /// How many shards stay open at once (the least recently used one closes). 0 is the default of 32.
    pub open_shards: usize,
    /// Collect every pipeline that the cache builds, so `save_archive` can write them.
    pub collect: bool,
    /// Compile the unspecialized pipeline of a pair of functions that the archive lacks, once, and make every
    /// pipeline of the pair whose state allows it by specializing that one (`cache::specialize`). Off: such a pair
    /// compiles each of its states from the functions.
    pub compile_bases: bool,
    /// Never specialize: look up the exact pipeline in the archive, else compile it from the functions.
    pub exact_only: bool,
    /// Archives that the host wrote in earlier runs (the runtime cache): extra lookup archives for every compile of the
    /// cache, beside the build's archive or shards. One that cannot be opened is reported and left out.
    pub runtime: Vec<PathBuf>,
}

/// Where a pipeline of [`PipelineCache::render_traced`] came from, and what making it cost. A time is the whole call:
/// archive lookups that missed count in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The cache already held it.
    Cached,
    /// The archive had it with this very state. Nothing was compiled.
    Archive(Duration),
    /// The archive had the unspecialized pipeline of its pair of functions, and the compiler specialized it to the
    /// colour state of the draw (`newRenderPipelineStateBySpecializationWithDescriptor`): no compile from the
    /// functions.
    Specialized(Duration),
    /// The archive lacked it (or there is none) and Metal compiled it from the functions.
    Compiled(Duration),
}

impl Source {
    /// The time the pipeline took to make; zero for one the cache held.
    pub fn took(self) -> Duration {
        match self {
            Self::Cached => Duration::ZERO,
            Self::Archive(t) | Self::Specialized(t) | Self::Compiled(t) => t,
        }
    }
}

/// Why the archive lacked a render pipeline, found by asking it again with the pipeline's state taken apart
/// ([`PipelineCache::explain_miss`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MissWhy {
    /// The archive has the pair of functions with the state the build assumed (unspecialized colour attachments,
    /// no vertex layout, no class, one sample). False: the pair is not in the archive at all.
    pub pair_archived: bool,
    /// The state groups that make the archive miss when set alone on the archived pair, named `topology`,
    /// `samples`, `alpha-to-coverage`, `vertex-layout`, `color-formats`, `blend`, `write-mask` and `icb`. Only
    /// groups the pipeline really sets are asked.
    pub rejected: Vec<&'static str>,
    /// The state is not the plain one (a vertex layout, samples, alpha to coverage, ICB support or a class with a
    /// layout), so the archive was asked for the base of that state with unspecialized colours.
    pub state_checked: bool,
    /// That base is in the archive (the lookup of the host hits, and it is not a miss of the archive).
    pub state_archived: bool,
    /// When it is not: the state groups that make the lookup hit when reset to the plain value alone, so the archive
    /// holds the state with that group different.
    pub state_differs: Vec<&'static str>,
}

/// A compiler with the serializer that is bound to it when the compiler is made.
struct Generation {
    compiler: RawCompiler,
    serializer: Option<Retained<ProtocolObject<dyn MTL4PipelineDataSetSerializer>>>,
}

// SAFETY: `MTL4Compiler` accepts calls from several threads, and the serializer is only called with the generation
// quiet (`PipelineCache::rotate_archive`) or by the owner of the cache.
unsafe impl Send for Generation {}
// SAFETY: see `Send`.
unsafe impl Sync for Generation {}

impl Generation {
    fn new(ctx: &Context, collect: bool) -> Result<Self> {
        let desc = MTL4CompilerDescriptor::new();
        let serializer = collect.then(|| {
            let sd = MTL4PipelineDataSetSerializerDescriptor::new();
            sd.setConfiguration(MTL4PipelineDataSetSerializerConfiguration::CaptureBinaries);
            ctx.device().newPipelineDataSetSerializerWithDescriptor(&sd)
        });
        desc.setPipelineDataSetSerializer(serializer.as_deref());
        let compiler = ctx
            .device()
            .newCompilerWithDescriptor_error(&desc)
            .map_err(|e| create("pipeline compiler", e.localizedDescription()))?;
        Ok(Self {
            compiler,
            serializer,
        })
    }

    fn write(&self, path: &Path) -> Result<()> {
        let s = self
            .serializer
            .as_ref()
            .ok_or(MetalError::Invalid("the cache was made without `collect`"))?;
        let p = path
            .to_str()
            .ok_or(MetalError::Invalid("the archive path is not UTF-8"))?;
        s.serializeAsArchiveAndFlushToURL_error(&NSURL::fileURLWithPath(&ns(p)))
            .map_err(|e| create("pipeline archive file", e.localizedDescription()))
    }
}

enum Entry {
    Compute(ComputePipeline),
    Render(RenderPipeline),
}

/// Pipeline states under a caller-chosen hash.
pub struct PipelineCache {
    /// The compiler that new compiles use, with the serializer bound to it. [`PipelineCache::rotate_archive`] swaps it.
    current: RwLock<Arc<Generation>>,
    /// The generations that were swapped out. Pipelines they made stay valid for as long as the cache lives.
    retired: Mutex<Vec<Arc<Generation>>>,
    ctx: Context,
    /// Whether the compilers capture into a serializer.
    collecting: bool,
    /// Pipelines inserted since the serializer was made.
    captured: AtomicU64,
    options: Retained<MTL4CompilerTaskOptions>,
    loaded_archive: bool,
    /// [`ArchiveConfig::compile_bases`].
    compile_bases: bool,
    /// [`ArchiveConfig::exact_only`].
    exact_only: bool,
    /// The archive of `load`, asked first for a render pipeline.
    archive: Option<Retained<ProtocolObject<dyn MTL4Archive>>>,
    /// The file of `archive` in the memory counters.
    _archive_bytes: Option<std::sync::Arc<crate::memstats::Tracked>>,
    /// The archives of `ArchiveConfig::runtime`, which are lookup archives of every compile.
    _runtime: Vec<shards::Open>,
    /// The shards of `shards`: asked in place of `archive`.
    shards: Option<ShardSet<shards::Open>>,
    /// A read-write lock: every encode thread looks pipelines up at once, and they are built rarely.
    entries: RwLock<HashMap<u64, Entry>>,
    hits: AtomicU64,
    builds: AtomicU64,
    /// The unspecialized pipelines of the archive by pair of functions, and how specializing them went.
    bases: Bases,
    /// The library of [`NOP_FRAGMENT`], compiled at the first depth-only mesh pipeline.
    nop: OnceLock<std::result::Result<ShaderLibrary, String>>,
}

// SAFETY: `MTL4Compiler` accepts calls from several threads, the entry map has a lock and the counters are atomic.
unsafe impl Send for PipelineCache {}
// SAFETY: see `Send`.
unsafe impl Sync for PipelineCache {}

impl PipelineCache {
    /// Makes a cache with its own compiler.
    pub fn new(ctx: &Context, archive: &ArchiveConfig) -> Result<Self> {
        let first = Generation::new(ctx, archive.collect)?;
        let options = MTL4CompilerTaskOptions::new();
        let mut loaded_archive = false;
        let mut archive_obj = None;
        let mut archive_bytes = None;
        let shard_set = archive
            .shards
            .as_deref()
            .and_then(|d| ShardSet::open(ctx, d, archive.open_shards));
        if shard_set.is_some() {
            loaded_archive = true;
        } else if let Some(path) = archive.load.as_deref().filter(|p| p.exists()) {
            let open =
                shards::open_archive(ctx, path).map_err(|e| create("pipeline archive", e))?;
            options.setLookupArchives(Some(&NSArray::from_retained_slice(std::slice::from_ref(
                &open.archive,
            ))));
            archive_obj = Some(open.archive.clone());
            archive_bytes = Some(open.tracked());
            loaded_archive = true;
        }
        let runtime = shards::open_runtime(ctx, &archive.runtime);
        if !runtime.is_empty() {
            let mut all: Vec<_> = archive_obj.iter().cloned().collect();
            all.extend(runtime.iter().map(|o| o.archive.clone()));
            options.setLookupArchives(Some(&NSArray::from_retained_slice(&all)));
        }
        Ok(Self {
            current: RwLock::new(Arc::new(first)),
            retired: Mutex::new(Vec::new()),
            ctx: ctx.clone(),
            collecting: archive.collect,
            captured: AtomicU64::new(0),
            options,
            loaded_archive,
            compile_bases: archive.compile_bases,
            exact_only: archive.exact_only,
            archive: archive_obj,
            _archive_bytes: archive_bytes,
            _runtime: runtime,
            shards: shard_set,
            entries: RwLock::new(HashMap::new()),
            hits: AtomicU64::new(0),
            builds: AtomicU64::new(0),
            bases: Bases::default(),
            nop: OnceLock::new(),
        })
    }

    /// The compute pipeline for `key`. The first call compiles `function` of `lib`. Later calls return the state.
    pub fn compute(
        &self,
        key: u64,
        lib: &ShaderLibrary,
        function: &str,
    ) -> Result<ComputePipeline> {
        self.compute_with(key, lib, function, false)
    }

    /// Like [`PipelineCache::compute`], for a pipeline that a compute indirect command buffer may use.
    pub fn compute_icb(
        &self,
        key: u64,
        lib: &ShaderLibrary,
        function: &str,
    ) -> Result<ComputePipeline> {
        self.compute_with(key, lib, function, true)
    }

    fn compute_with(
        &self,
        key: u64,
        lib: &ShaderLibrary,
        function: &str,
        support_icb: bool,
    ) -> Result<ComputePipeline> {
        if let Some(Entry::Compute(p)) = self.lookup(key)? {
            return Ok(p);
        }
        let d = MTL4ComputePipelineDescriptor::new();
        let f = lib.function(function)?;
        d.setComputeFunctionDescriptor(Some(&f));
        if support_icb {
            d.setSupportIndirectCommandBuffers(MTL4IndirectCommandBufferSupportState::Enabled);
        }
        // a shard has its compute pipelines: the compiler finds them in the shard's archive
        let options = self
            .shards
            .as_ref()
            .and_then(|s| s.compute_options(lib, function));
        let raw = self
            .generation()
            .compiler
            .newComputePipelineStateWithDescriptor_compilerTaskOptions_error(
                &d,
                Some(options.as_deref().unwrap_or(&self.options)),
            )
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let p = ComputePipeline { raw };
        self.insert(key, Entry::Compute(p.clone()))?;
        Ok(p)
    }

    /// The render pipeline for `key`. The first call compiles it.
    pub fn render(&self, key: u64, desc: &RenderPipelineDesc<'_>) -> Result<RenderPipeline> {
        self.render_traced(key, desc).map(|r| r.0)
    }

    /// Like [`PipelineCache::render`], and tells where the pipeline came from and what it cost. The archive is asked
    /// first: the unspecialized pipeline of the pair, specialized to the draw's colour state when the rest of the
    /// state is the one the build archived, then the exact state. Only when both miss does Metal compile the functions
    /// (with [`ArchiveConfig::compile_bases`] the unspecialized pipeline of a pair the archive lacks is compiled once
    /// and specialized instead).
    pub fn render_traced(
        &self,
        key: u64,
        desc: &RenderPipelineDesc<'_>,
    ) -> Result<(RenderPipeline, Source)> {
        if let Some(Entry::Render(p)) = self.lookup(key)? {
            return Ok((p, Source::Cached));
        }
        let t0 = Instant::now();
        let archive = self.render_archive(desc);
        if let Some(raw) = self.specialize(archive.as_deref(), desc) {
            let p = RenderPipeline { raw };
            self.insert(key, Entry::Render(p.clone()))?;
            return Ok((p, Source::Specialized(t0.elapsed())));
        }
        let d = self.render_descriptor(desc)?;
        if let Some(a) = &archive
            && let Ok(raw) = a.newRenderPipelineStateWithDescriptor_error(&d)
        {
            let p = RenderPipeline { raw };
            self.insert(key, Entry::Render(p.clone()))?;
            return Ok((p, Source::Archive(t0.elapsed())));
        }
        let raw = self
            .generation()
            .compiler
            .newRenderPipelineStateWithDescriptor_compilerTaskOptions_error(&d, Some(&self.options))
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let p = RenderPipeline { raw };
        self.insert(key, Entry::Render(p.clone()))?;
        Ok((p, Source::Compiled(t0.elapsed())))
    }

    /// The archive to ask for a render pipeline: its shard when the cache has shards, else the single archive.
    fn render_archive(
        &self,
        desc: &RenderPipelineDesc<'_>,
    ) -> Option<Retained<ProtocolObject<dyn MTL4Archive>>> {
        match &self.shards {
            Some(s) => s.render_archive(desc),
            None => self.archive.clone(),
        }
    }

    /// The Metal descriptor of a render pipeline.
    fn render_descriptor(
        &self,
        desc: &RenderPipelineDesc<'_>,
    ) -> Result<Retained<MTL4RenderPipelineDescriptor>> {
        let d = MTL4RenderPipelineDescriptor::new();
        let vf = desc.vertex.0.function(desc.vertex.1)?;
        d.setVertexFunctionDescriptor(Some(&vf));
        if let Some((lib, name)) = desc.fragment {
            let ff = lib.function(name)?;
            d.setFragmentFunctionDescriptor(Some(&ff));
        }
        check_sample_count(desc.sample_count)?;
        if let Some(vd) = desc.vertex_layout.build() {
            d.setVertexDescriptor(Some(&vd));
        }
        d.setInputPrimitiveTopology(desc.topology.to_mtl());
        // SAFETY: the count was checked above and is a supported value.
        unsafe { d.setRasterSampleCount(desc.sample_count as usize) };
        d.setAlphaToCoverageState(if desc.alpha_to_coverage {
            MTL4AlphaToCoverageState::Enabled
        } else {
            MTL4AlphaToCoverageState::Disabled
        });
        d.setSupportIndirectCommandBuffers(if desc.support_icb {
            MTL4IndirectCommandBufferSupportState::Enabled
        } else {
            MTL4IndirectCommandBufferSupportState::Disabled
        });
        let colors = d.colorAttachments();
        for (i, c) in desc.colors.iter().enumerate() {
            let Some(c) = c else { continue };
            // SAFETY: the index is a plain attachment slot. Metal checks the limit when it compiles.
            let a = unsafe { colors.objectAtIndexedSubscript(i) };
            a.setPixelFormat(c.format.to_mtl());
            a.setWriteMask(c.mask());
            a.setBlendingState(c.blend_state());
            if let Some(b) = &c.blend {
                a.setSourceRGBBlendFactor(b.src_rgb.to_mtl());
                a.setDestinationRGBBlendFactor(b.dst_rgb.to_mtl());
                a.setRgbBlendOperation(b.op_rgb.to_mtl());
                a.setSourceAlphaBlendFactor(b.src_alpha.to_mtl());
                a.setDestinationAlphaBlendFactor(b.dst_alpha.to_mtl());
                a.setAlphaBlendOperation(b.op_alpha.to_mtl());
            }
        }
        Ok(d)
    }

    /// The mesh render pipeline for `key`. The first call compiles it. The pipeline draws with
    /// [`RenderCommand::DrawMeshThreadgroups`](crate::RenderCommand::DrawMeshThreadgroups) and its indirect form.
    pub fn mesh(&self, key: u64, desc: &MeshPipelineDesc<'_>) -> Result<RenderPipeline> {
        self.mesh_traced(key, desc).map(|r| r.0)
    }

    /// Like [`PipelineCache::mesh`], and tells where the pipeline came from. The build's archive has no mesh pipeline,
    /// so the answer is `Cached` or `Compiled`.
    pub fn mesh_traced(
        &self,
        key: u64,
        desc: &MeshPipelineDesc<'_>,
    ) -> Result<(RenderPipeline, Source)> {
        if let Some(Entry::Render(p)) = self.lookup(key)? {
            return Ok((p, Source::Cached));
        }
        let d = MTL4MeshRenderPipelineDescriptor::new();
        if let Some((lib, name)) = desc.object {
            let of = lib.function(name)?;
            d.setObjectFunctionDescriptor(Some(&of));
            d.setPayloadMemoryLength(desc.payload_bytes.max(16).next_multiple_of(16) as usize);
            if desc.object_threads != 0 {
                d.setMaxTotalThreadsPerObjectThreadgroup(desc.object_threads as usize);
            }
        }
        let mf = desc.mesh.0.function(desc.mesh.1)?;
        d.setMeshFunctionDescriptor(Some(&mf));
        if desc.mesh_threads != 0 {
            d.setMaxTotalThreadsPerMeshThreadgroup(desc.mesh_threads as usize);
        }
        let nop;
        let (flib, fname) = match desc.fragment {
            Some(f) => f,
            None => {
                nop = self.nop_fragment()?;
                (&nop, NOP_FRAGMENT_NAME)
            }
        };
        let ff = flib.function(fname)?;
        d.setFragmentFunctionDescriptor(Some(&ff));
        check_sample_count(desc.sample_count)?;
        // SAFETY: the count was checked above and is a supported value.
        unsafe { d.setRasterSampleCount(desc.sample_count as usize) };
        d.setAlphaToCoverageState(if desc.alpha_to_coverage {
            MTL4AlphaToCoverageState::Enabled
        } else {
            MTL4AlphaToCoverageState::Disabled
        });
        let colors = d.colorAttachments();
        for (i, c) in desc.colors.iter().enumerate() {
            let Some(c) = c else { continue };
            // SAFETY: the index is a plain attachment slot. Metal checks the limit when it compiles.
            let a = unsafe { colors.objectAtIndexedSubscript(i) };
            a.setPixelFormat(c.format.to_mtl());
            a.setWriteMask(c.mask());
            a.setBlendingState(c.blend_state());
            if let Some(b) = &c.blend {
                a.setSourceRGBBlendFactor(b.src_rgb.to_mtl());
                a.setDestinationRGBBlendFactor(b.dst_rgb.to_mtl());
                a.setRgbBlendOperation(b.op_rgb.to_mtl());
                a.setSourceAlphaBlendFactor(b.src_alpha.to_mtl());
                a.setDestinationAlphaBlendFactor(b.dst_alpha.to_mtl());
                a.setAlphaBlendOperation(b.op_alpha.to_mtl());
            }
        }
        let t0 = Instant::now();
        let raw = self
            .generation()
            .compiler
            .newRenderPipelineStateWithDescriptor_compilerTaskOptions_error(&d, Some(&self.options))
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let took = t0.elapsed();
        let p = RenderPipeline { raw };
        self.insert(key, Entry::Render(p.clone()))?;
        Ok((p, Source::Compiled(took)))
    }

    /// The library of the empty fragment function, compiled once.
    fn nop_fragment(&self) -> Result<ShaderLibrary> {
        let r = self.nop.get_or_init(|| {
            let d = MTL4LibraryDescriptor::new();
            d.setSource(Some(&ns(NOP_FRAGMENT)));
            d.setName(Some(&ns("nxkit-mesh-nop")));
            self.generation()
                .compiler
                .newLibraryWithDescriptor_error(&d)
                .map(ShaderLibrary::tracked)
                .map_err(|e| e.localizedDescription().to_string())
        });
        r.clone().map_err(MetalError::Compile)
    }

    /// The render pipeline under `key` when the cache already holds it. Nothing is compiled.
    pub fn cached_render(&self, key: u64) -> Option<RenderPipeline> {
        match self.lookup(key) {
            Ok(Some(Entry::Render(p))) => Some(p),
            _ => None,
        }
    }

    fn lookup(&self, key: u64) -> Result<Option<Entry>> {
        let map = self
            .entries
            .read()
            .map_err(|_| MetalError::Invalid("pipeline cache lock poisoned"))?;
        let found = map.get(&key).map(|e| match e {
            Entry::Compute(p) => Entry::Compute(p.clone()),
            Entry::Render(p) => Entry::Render(p.clone()),
        });
        if found.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        Ok(found)
    }

    fn insert(&self, key: u64, e: Entry) -> Result<()> {
        self.builds.fetch_add(1, Ordering::Relaxed);
        if self.collecting {
            self.captured.fetch_add(1, Ordering::Relaxed);
        }
        let fresh = self
            .entries
            .write()
            .map_err(|_| MetalError::Invalid("pipeline cache lock poisoned"))?
            .insert(key, e)
            .is_none();
        if fresh {
            crate::memstats::pipeline_kept();
        }
        Ok(())
    }

    /// The compiler and serializer in use now. A compile holds the `Arc` until it is done, which is how
    /// [`PipelineCache::rotate_archive`] knows that a swapped-out generation is quiet.
    fn generation(&self) -> Arc<Generation> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Writes the archive of every pipeline the current serializer captured. The cache must have been made with
    /// `collect`. A long run should use [`PipelineCache::rotate_archive`]: a serializer has a store of limited size.
    pub fn save_archive(&self, path: &Path) -> Result<()> {
        self.generation().write(path)
    }

    /// The pipelines the current serializer has captured, about: the count of pipelines inserted since it was made.
    pub fn captured(&self) -> u64 {
        self.captured.load(Ordering::Relaxed)
    }

    /// Hands the capture over: from now on new compiles run on a new compiler with a fresh serializer, the old
    /// serializer waits (up to 20 s) until the compiles that were running on its compiler are done, and then writes its
    /// archive to `path`. The serializer's internal store is limited (about 10 MB); a full one hands Metal nil data and
    /// the compiler crashes, so the host rotates well before. If compiles are still running after the wait, nothing is
    /// written (their capture is lost, but the serializer is not touched under them).
    pub fn rotate_archive(&self, path: &Path) -> Result<()> {
        if !self.collecting {
            return Err(MetalError::Invalid("the cache was made without `collect`"));
        }
        let fresh = Arc::new(Generation::new(&self.ctx, true)?);
        let old = std::mem::replace(
            &mut *self.current.write().unwrap_or_else(PoisonError::into_inner),
            fresh,
        );
        self.captured.store(0, Ordering::Relaxed);
        let t0 = Instant::now();
        while Arc::strong_count(&old) > 1 && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let quiet = Arc::strong_count(&old) == 1;
        let written = if quiet {
            old.write(path)
        } else {
            Err(MetalError::Invalid("compiles were still running"))
        };
        self.retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(old);
        written
    }
}
