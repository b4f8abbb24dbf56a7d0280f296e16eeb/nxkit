//! Shader libraries and pipeline states, built through `MTL4Compiler`.
//!
//! A pipeline cache keeps the states under a hash that the caller chooses. It can also write every pipeline it built
//! to a binary archive on disk and read that archive back, so a second run skips the compile.

use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSURL;
use objc2_metal::{
    MTL4Compiler, MTL4LibraryDescriptor, MTL4LibraryFunctionDescriptor, MTLComputePipelineState,
    MTLDevice, MTLLibrary, MTLRenderPipelineState,
};

use crate::device::{Context, ns};
use crate::error::{MetalError, Result};
use crate::format::Format;
use crate::state::{ColorTarget, TopologyClass, VertexDescriptor};

/// A compiled shader library.
#[derive(Clone)]
pub struct ShaderLibrary {
    raw: Retained<ProtocolObject<dyn MTLLibrary>>,
    /// What the shard index names the library by: the file stem of a `.metallib` (`<key>_b`), or the name of the source.
    stem: Option<std::sync::Arc<str>>,
    /// The library in the memory counters, shared by the clones.
    _tracked: std::sync::Arc<crate::memstats::Tracked>,
}

// SAFETY: a library is an immutable, thread safe Metal object.
unsafe impl Send for ShaderLibrary {}
// SAFETY: see `Send`.
unsafe impl Sync for ShaderLibrary {}

impl ShaderLibrary {
    pub(crate) fn tracked(raw: Retained<ProtocolObject<dyn MTLLibrary>>) -> Self {
        Self {
            raw,
            stem: None,
            _tracked: std::sync::Arc::new(crate::memstats::Tracked::new(
                crate::memstats::Kind::Library,
                0,
            )),
        }
    }

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
        let mut lib = Self::tracked(raw);
        lib.stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(std::sync::Arc::from);
        Ok(lib)
    }

    /// What the shard index names the library by: the file stem of a `.metallib`, or the name of a source library.
    pub fn stem(&self) -> Option<&str> {
        self.stem.as_deref()
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
        let mut lib = Self::tracked(raw);
        lib.stem = Some(std::sync::Arc::from(name));
        Ok(lib)
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
            colors: color_formats
                .iter()
                .map(|f| Some(ColorTarget::new(*f)))
                .collect(),
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

pub use cache::{ArchiveConfig, BaseStats, MissWhy, PipelineCache, Source};

mod cache;
mod lru;
mod shards;
