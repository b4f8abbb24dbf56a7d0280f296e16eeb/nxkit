//! Shader libraries and pipeline states, built through `MTL4Compiler`.
//!
//! A pipeline cache keeps the states under a hash that the caller chooses. It can also write every pipeline it built
//! to a binary archive on disk and read that archive back, so a second run skips the compile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSArray, NSURL};
use objc2_metal::{
    MTL4AlphaToCoverageState, MTL4Compiler, MTL4CompilerDescriptor, MTL4CompilerTaskOptions,
    MTL4ComputePipelineDescriptor, MTL4IndirectCommandBufferSupportState, MTL4LibraryDescriptor,
    MTL4LibraryFunctionDescriptor, MTL4MeshRenderPipelineDescriptor, MTL4PipelineDataSetSerializer,
    MTL4PipelineDataSetSerializerConfiguration, MTL4PipelineDataSetSerializerDescriptor,
    MTL4RenderPipelineDescriptor, MTLComputePipelineState, MTLDevice, MTLLibrary,
    MTLRenderPipelineState,
};

use crate::device::{Context, RawCompiler, ns};
use crate::error::{MetalError, Result, create};
use crate::format::Format;
use crate::state::{ColorTarget, TopologyClass, VertexDescriptor, check_sample_count};

/// A compiled shader library.
#[derive(Clone)]
pub struct ShaderLibrary {
    raw: Retained<ProtocolObject<dyn MTLLibrary>>,
}

// SAFETY: a library is an immutable, thread safe Metal object.
unsafe impl Send for ShaderLibrary {}
// SAFETY: see `Send`.
unsafe impl Sync for ShaderLibrary {}

impl ShaderLibrary {
    /// Loads a `.metallib` file.
    pub fn from_path(ctx: &Context, path: &Path) -> Result<Self> {
        let p = path
            .to_str()
            .ok_or(MetalError::Invalid("the library path is not UTF-8"))?;
        let url = NSURL::fileURLWithPath(&ns(p));
        let raw = ctx
            .device()
            .newLibraryWithURL_error(&url)
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        Ok(Self { raw })
    }

    /// Compiles Metal Shading Language source with `MTL4Compiler`.
    pub fn from_source(ctx: &Context, name: &str, source: &str) -> Result<Self> {
        let d = MTL4LibraryDescriptor::new();
        d.setSource(Some(&ns(source)));
        d.setName(Some(&ns(name)));
        let raw = ctx
            .compiler()
            .newLibraryWithDescriptor_error(&d)
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        Ok(Self { raw })
    }

    /// The names of the functions in the library.
    pub fn function_names(&self) -> Vec<String> {
        self.raw
            .functionNames()
            .iter()
            .map(|n| n.to_string())
            .collect()
    }

    fn function(&self, name: &str) -> Result<Retained<MTL4LibraryFunctionDescriptor>> {
        if !self.function_names().iter().any(|n| n == name) {
            return Err(MetalError::Compile(format!(
                "no function named {name} in the library"
            )));
        }
        let f = MTL4LibraryFunctionDescriptor::new();
        f.setName(Some(&ns(name)));
        f.setLibrary(Some(&self.raw));
        Ok(f)
    }
}

/// A compute pipeline state.
#[derive(Clone)]
pub struct ComputePipeline {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
}

impl ComputePipeline {
    /// The widest threadgroup the pipeline allows.
    pub fn max_threads_per_threadgroup(&self) -> u64 {
        self.raw.maxTotalThreadsPerThreadgroup() as u64
    }

    /// The width of a SIMD group on this GPU.
    pub fn thread_execution_width(&self) -> u64 {
        self.raw.threadExecutionWidth() as u64
    }
}

/// A render pipeline state.
#[derive(Clone)]
pub struct RenderPipeline {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
}

// SAFETY: a pipeline state is an immutable, thread safe Metal object.
unsafe impl Send for RenderPipeline {}
// SAFETY: see `Send`.
unsafe impl Sync for RenderPipeline {}

// SAFETY: a pipeline state is an immutable, thread safe Metal object.
unsafe impl Send for ComputePipeline {}
// SAFETY: see `Send`.
unsafe impl Sync for ComputePipeline {}

/// What a render pipeline needs.
#[derive(Clone)]
pub struct RenderPipelineDesc<'a> {
    /// The library and function name of the vertex stage.
    pub vertex: (&'a ShaderLibrary, &'a str),
    /// The library and function name of the fragment stage, or none for a depth-only pipeline.
    pub fragment: Option<(&'a ShaderLibrary, &'a str)>,
    /// The colour attachments by slot, with their blending. `None` is an empty slot.
    pub colors: Vec<Option<ColorTarget>>,
    /// The vertex input. Empty when the shader fetches its own vertices.
    pub vertex_layout: VertexDescriptor,
    /// The primitive class the pipeline draws.
    pub topology: TopologyClass,
    /// The samples per pixel: 1, 2, 4 or 8.
    pub sample_count: u32,
    /// Whether the fragment coverage comes from its alpha.
    pub alpha_to_coverage: bool,
    /// Whether an indirect command buffer may use the pipeline. Metal needs the flag at build time.
    pub support_icb: bool,
}

impl<'a> RenderPipelineDesc<'a> {
    /// A triangle pipeline without blending, vertex layout or multisampling.
    pub fn new(
        vertex: (&'a ShaderLibrary, &'a str),
        fragment: Option<(&'a ShaderLibrary, &'a str)>,
        color_formats: &[Format],
    ) -> Self {
        Self {
            vertex,
            fragment,
            colors: color_formats.iter().map(|f| Some(ColorTarget::new(*f))).collect(),
            vertex_layout: VertexDescriptor::default(),
            topology: TopologyClass::Triangle,
            sample_count: 1,
            alpha_to_coverage: false,
            support_icb: false,
        }
    }
}

/// What a mesh render pipeline needs: an NVN task shader as the object stage, the mesh shader and the pixel shader.
#[derive(Clone)]
pub struct MeshPipelineDesc<'a> {
    /// The library and function name of the object stage, when the program has a task shader.
    pub object: Option<(&'a ShaderLibrary, &'a str)>,
    /// The library and function name of the mesh stage.
    pub mesh: (&'a ShaderLibrary, &'a str),
    /// The library and function name of the fragment stage. Metal needs a fragment function in a mesh pipeline, so
    /// `None` uses an empty one and the pipeline draws depth only.
    pub fragment: Option<(&'a ShaderLibrary, &'a str)>,
    /// The colour attachments by slot, with their blending. `None` is an empty slot.
    pub colors: Vec<Option<ColorTarget>>,
    /// The samples per pixel: 1, 2, 4 or 8.
    pub sample_count: u32,
    /// Whether the fragment coverage comes from its alpha.
    pub alpha_to_coverage: bool,
    /// The most threads of an object threadgroup. 0 leaves it to the function.
    pub object_threads: u32,
    /// The most threads of a mesh threadgroup. 0 leaves it to the function.
    pub mesh_threads: u32,
    /// The bytes of the payload that the object stage hands to the mesh stage. Rounded up to 16.
    pub payload_bytes: u32,
}

/// The empty fragment function of a depth-only mesh pipeline.
const NOP_FRAGMENT: &str = "fragment void nxkit_mesh_nop() {}";
const NOP_FRAGMENT_NAME: &str = "nxkit_mesh_nop";

/// Where a pipeline cache reads and writes its archive.
#[derive(Clone, Debug, Default)]
pub struct ArchiveConfig {
    /// An archive to look pipelines up in before compiling. A missing file is not an error.
    pub load: Option<PathBuf>,
    /// Collect every pipeline that the cache builds, so `save_archive` can write them.
    pub collect: bool,
}

enum Entry {
    Compute(ComputePipeline),
    Render(RenderPipeline),
}

/// Pipeline states under a caller-chosen hash.
pub struct PipelineCache {
    compiler: RawCompiler,
    serializer: Option<Retained<ProtocolObject<dyn MTL4PipelineDataSetSerializer>>>,
    options: Retained<MTL4CompilerTaskOptions>,
    loaded_archive: bool,
    /// A read-write lock: every encode thread looks pipelines up at once, and they are built rarely.
    entries: RwLock<HashMap<u64, Entry>>,
    hits: AtomicU64,
    builds: AtomicU64,
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
        let desc = MTL4CompilerDescriptor::new();
        let serializer = archive.collect.then(|| {
            let sd = MTL4PipelineDataSetSerializerDescriptor::new();
            sd.setConfiguration(MTL4PipelineDataSetSerializerConfiguration::CaptureBinaries);
            ctx.device().newPipelineDataSetSerializerWithDescriptor(&sd)
        });
        desc.setPipelineDataSetSerializer(serializer.as_deref());
        let compiler = ctx
            .device()
            .newCompilerWithDescriptor_error(&desc)
            .map_err(|e| create("pipeline compiler", e.localizedDescription()))?;
        let options = MTL4CompilerTaskOptions::new();
        let mut loaded_archive = false;
        if let Some(path) = archive.load.as_deref().filter(|p| p.exists()) {
            let p = path
                .to_str()
                .ok_or(MetalError::Invalid("the archive path is not UTF-8"))?;
            let a = ctx
                .device()
                .newArchiveWithURL_error(&NSURL::fileURLWithPath(&ns(p)))
                .map_err(|e| create("pipeline archive", e.localizedDescription()))?;
            options.setLookupArchives(Some(&NSArray::from_retained_slice(&[a])));
            loaded_archive = true;
        }
        Ok(Self {
            compiler,
            serializer,
            options,
            loaded_archive,
            entries: RwLock::new(HashMap::new()),
            hits: AtomicU64::new(0),
            builds: AtomicU64::new(0),
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
        let raw = self
            .compiler
            .newComputePipelineStateWithDescriptor_compilerTaskOptions_error(
                &d,
                Some(&self.options),
            )
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let p = ComputePipeline { raw };
        self.insert(key, Entry::Compute(p.clone()))?;
        Ok(p)
    }

    /// The render pipeline for `key`. The first call compiles it.
    pub fn render(&self, key: u64, desc: &RenderPipelineDesc<'_>) -> Result<RenderPipeline> {
        if let Some(Entry::Render(p)) = self.lookup(key)? {
            return Ok(p);
        }
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
        let raw = self
            .compiler
            .newRenderPipelineStateWithDescriptor_compilerTaskOptions_error(&d, Some(&self.options))
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let p = RenderPipeline { raw };
        self.insert(key, Entry::Render(p.clone()))?;
        Ok(p)
    }

    /// The mesh render pipeline for `key`. The first call compiles it. The pipeline draws with
    /// [`RenderCommand::DrawMeshThreadgroups`](crate::RenderCommand::DrawMeshThreadgroups) and its indirect form.
    pub fn mesh(&self, key: u64, desc: &MeshPipelineDesc<'_>) -> Result<RenderPipeline> {
        if let Some(Entry::Render(p)) = self.lookup(key)? {
            return Ok(p);
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
        let raw = self
            .compiler
            .newRenderPipelineStateWithDescriptor_compilerTaskOptions_error(&d, Some(&self.options))
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let p = RenderPipeline { raw };
        self.insert(key, Entry::Render(p.clone()))?;
        Ok(p)
    }

    /// The library of the empty fragment function, compiled once.
    fn nop_fragment(&self) -> Result<ShaderLibrary> {
        let r = self.nop.get_or_init(|| {
            let d = MTL4LibraryDescriptor::new();
            d.setSource(Some(&ns(NOP_FRAGMENT)));
            d.setName(Some(&ns("nxkit-mesh-nop")));
            self.compiler
                .newLibraryWithDescriptor_error(&d)
                .map(|raw| ShaderLibrary { raw })
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
        self.entries
            .write()
            .map_err(|_| MetalError::Invalid("pipeline cache lock poisoned"))?
            .insert(key, e);
        Ok(())
    }

    /// Writes the archive of every pipeline built so far. The cache must have been made with `collect`.
    pub fn save_archive(&self, path: &Path) -> Result<()> {
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

    /// The number of lookups that found a pipeline.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// The number of pipelines built through the compiler (an archive hit counts too).
    pub fn builds(&self) -> u64 {
        self.builds.load(Ordering::Relaxed)
    }

    /// True when an archive was found and attached.
    pub fn has_archive(&self) -> bool {
        self.loaded_archive
    }
}
