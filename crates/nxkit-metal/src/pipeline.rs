//! Shader libraries and pipeline states, built through `MTL4Compiler`.
//!
//! A pipeline cache keeps the states under a hash that the caller chooses. It can also write every pipeline it built
//! to a binary archive on disk and read that archive back, so a second run skips the compile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSArray, NSURL};
use objc2_metal::{
    MTL4Compiler, MTL4CompilerDescriptor, MTL4CompilerTaskOptions, MTL4ComputePipelineDescriptor,
    MTL4LibraryDescriptor, MTL4LibraryFunctionDescriptor, MTL4PipelineDataSetSerializer,
    MTL4PipelineDataSetSerializerConfiguration, MTL4PipelineDataSetSerializerDescriptor,
    MTL4RenderPipelineDescriptor, MTLComputePipelineState, MTLDevice, MTLLibrary,
    MTLRenderPipelineState,
};

use crate::device::{Context, RawCompiler, ns};
use crate::error::{MetalError, Result, create};
use crate::format::Format;

/// A compiled shader library.
#[derive(Clone)]
pub struct ShaderLibrary {
    raw: Retained<ProtocolObject<dyn MTLLibrary>>,
}

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

/// What a render pipeline needs. Blending, depth and the vertex layout come with the frame format later.
#[derive(Clone)]
pub struct RenderPipelineDesc<'a> {
    /// The library and function name of the vertex stage.
    pub vertex: (&'a ShaderLibrary, &'a str),
    /// The library and function name of the fragment stage, or none for a depth-only pipeline.
    pub fragment: Option<(&'a ShaderLibrary, &'a str)>,
    /// The formats of the colour attachments, in order.
    pub color_formats: Vec<Format>,
}

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
    entries: Mutex<HashMap<u64, Entry>>,
    hits: AtomicU64,
    builds: AtomicU64,
}

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
            entries: Mutex::new(HashMap::new()),
            hits: AtomicU64::new(0),
            builds: AtomicU64::new(0),
        })
    }

    /// The compute pipeline for `key`. The first call compiles `function` of `lib`. Later calls return the state.
    pub fn compute(
        &self,
        key: u64,
        lib: &ShaderLibrary,
        function: &str,
    ) -> Result<ComputePipeline> {
        if let Some(Entry::Compute(p)) = self.lookup(key)? {
            return Ok(p);
        }
        let d = MTL4ComputePipelineDescriptor::new();
        let f = lib.function(function)?;
        d.setComputeFunctionDescriptor(Some(&f));
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
        let colors = d.colorAttachments();
        for (i, f) in desc.color_formats.iter().enumerate() {
            // SAFETY: the index is a plain attachment slot. Metal checks the limit when it compiles.
            let a = unsafe { colors.objectAtIndexedSubscript(i) };
            a.setPixelFormat(f.to_mtl());
        }
        let raw = self
            .compiler
            .newRenderPipelineStateWithDescriptor_compilerTaskOptions_error(&d, Some(&self.options))
            .map_err(|e| MetalError::Compile(e.localizedDescription().to_string()))?;
        let p = RenderPipeline { raw };
        self.insert(key, Entry::Render(p.clone()))?;
        Ok(p)
    }

    fn lookup(&self, key: u64) -> Result<Option<Entry>> {
        let map = self
            .entries
            .lock()
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
            .lock()
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
