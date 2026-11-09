//! Pipelines keyed by their translated state (R1, R17).
//!
//! The key is a hash of everything that goes into the Metal pipeline descriptor: the two shader keys, the colour
//! targets with their blend state and write mask, the vertex layout, the topology class, the sample count and the
//! alpha-to-coverage flag. Two draws with the same translated state share one pipeline, however different their NVN
//! state was. The depth and stencil state is a separate object that Metal binds per draw and not per pipeline.

mod render;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{
    ArchiveConfig, ComputePipeline, Context, DepthStencilDesc, DepthStencilState, Format,
    MeshPipelineDesc, PipelineCache, RenderPipeline, RenderPipelineDesc, ShaderLibrary, Source,
    TopologyClass,
};

use crate::detail::{AttributeDetail, BlendDetail, DrawDetail, ProgramRef};
use crate::programs::StageProgram;
use crate::stats::Skip;
use crate::translate;

/// Counts a pipeline that Metal did not build for the health report and prints it under the `pipe` category (at most
/// 300 lines). Gives back `Skip::Pipeline`.
fn pipe_failed(what: std::fmt::Arguments<'_>) -> Skip {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::PipelineFailed, what);
    if crate::diag("pipe") && crate::diag::first(&SHOWN, 300) {
        eprintln!("DIAG pipe {what}");
    }
    Skip::Pipeline
}

/// The formats of the colour attachments of a pass by slot, and its sample count.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Targets {
    /// The format of each colour slot. A slot without attachment is `None`.
    pub(crate) colors: Vec<Option<Format>>,
    /// The sample count of the attachments.
    pub(crate) samples: u32,
}

/// A map with the fast hasher. The keys of these maps are not attacker controlled.
type FastMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;
type FastSet<K> = HashSet<K, foldhash::fast::RandomState>;

/// The numbers of the [`Pipelines`] made so far, which tell the thread memos apart.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Everything of a draw that goes into the pipeline key, and the pipeline that came out. The encoder asks for the
/// pipeline of every draw and nearly always gets the answer of the draw before, so one compare replaces building the
/// descriptor and hashing it. A memo belongs to one thread.
struct Last {
    owner: u64,
    icb: bool,
    topology: u32,
    alpha_to_coverage: bool,
    program: ProgramRef,
    targets: Targets,
    blend: [BlendDetail; 8],
    write_masks: [u8; 8],
    attributes: Vec<AttributeDetail>,
    /// The index, stride and divisor of each stream. The address does not matter for the pipeline.
    streams: Vec<(u32, u32, u32)>,
    pipeline: RenderPipeline,
    key: u64,
}

impl Last {
    fn matches(&self, owner: u64, d: &DrawDetail, targets: &Targets, icb: bool) -> bool {
        self.owner == owner
            && self.icb == icb
            && self.topology == d.topology
            && self.alpha_to_coverage == d.raster.alpha_to_coverage
            && self.write_masks == d.write_masks
            && self.blend == d.blend
            && self.program == d.program
            && self.targets == *targets
            && self.attributes == d.attributes
            && self.streams.len() == d.streams.len()
            && self
                .streams
                .iter()
                .zip(&d.streams)
                .all(|(m, s)| *m == (s.index, s.stride, s.divisor))
    }
}

/// The depth and stencil state that the last draw of a thread used.
struct LastDepth {
    owner: u64,
    desc: DepthStencilDesc,
    state: DepthStencilState,
}

thread_local! {
    static LAST_PIPELINE: RefCell<Option<Last>> = const { RefCell::new(None) };
    static LAST_DEPTH: RefCell<Option<LastDepth>> = const { RefCell::new(None) };
}

/// The pipeline cache and the depth and stencil states.
pub(crate) struct Pipelines {
    id: u64,
    ctx: Context,
    cache: Arc<PipelineCache>,
    depth: Mutex<FastMap<DepthStencilDesc, DepthStencilState>>,
    flat: Mutex<Option<ShaderLibrary>>,
    /// The shader of the `paint` knobs.
    paint: Mutex<Option<ShaderLibrary>>,
    /// The libraries of the host's own kernels, by name.
    host: Mutex<FastMap<&'static str, ShaderLibrary>>,
    topology_fallbacks: Arc<Mutex<FastSet<u64>>>,
    /// Render pipelines that compile on the background threads, and those that failed there.
    builds: Arc<Builds>,
}

/// The state of the background compiles (`NXKIT_PIPELINE_ASYNC`, on by default).
#[derive(Default)]
struct Builds {
    pending: Mutex<FastSet<u64>>,
    failed: Mutex<FastSet<u64>>,
    queue: Mutex<Option<std::sync::mpsc::Sender<Job>>>,
}

/// A pipeline to compile in the background, with an owned copy of its description.
struct Job {
    key: u64,
    what: String,
    kind: JobKind,
}

enum JobKind {
    Render(RenderJob),
    Mesh(MeshJob),
}

/// A render pipeline of vertex and fragment stages.
struct RenderJob {
    vertex: (ShaderLibrary, String),
    fragment: Option<(ShaderLibrary, String)>,
    colors: Vec<Option<nxkit_metal::ColorTarget>>,
    vertex_layout: nxkit_metal::VertexDescriptor,
    topology: TopologyClass,
    sample_count: u32,
    alpha_to_coverage: bool,
}

/// A mesh pipeline: object, mesh and fragment stages.
struct MeshJob {
    object: Option<(ShaderLibrary, String)>,
    mesh: (ShaderLibrary, String),
    fragment: Option<(ShaderLibrary, String)>,
    colors: Vec<Option<nxkit_metal::ColorTarget>>,
    sample_count: u32,
    alpha_to_coverage: bool,
    object_threads: u32,
    mesh_threads: u32,
    payload_bytes: u32,
}

impl MeshJob {
    fn desc(&self) -> MeshPipelineDesc<'_> {
        fn fun((l, e): &(ShaderLibrary, String)) -> (&ShaderLibrary, &str) {
            (l, e.as_str())
        }
        MeshPipelineDesc {
            object: self.object.as_ref().map(fun),
            mesh: fun(&self.mesh),
            fragment: self.fragment.as_ref().map(fun),
            colors: self.colors.clone(),
            sample_count: self.sample_count,
            alpha_to_coverage: self.alpha_to_coverage,
            object_threads: self.object_threads,
            mesh_threads: self.mesh_threads,
            payload_bytes: self.payload_bytes,
        }
    }
}

/// The threads that compile render pipelines in the background.
const COMPILE_THREADS: usize = 2;

/// Whether a render pipeline that the cache lacks compiles in the background (`NXKIT_PIPELINE_ASYNC=0` turns it off).
/// The draw is left out until it is ready, a frame or two, instead of stalling the encode for the 5 to 30 ms of the
/// compile.
fn async_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // The unit tests build synchronously, so a call returns its pipeline.
    *ON.get_or_init(|| {
        !cfg!(test) && std::env::var("NXKIT_PIPELINE_ASYNC").map_or(true, |v| v != "0")
    })
}

/// The point size fallback of [`build_render`] has this key.
const FALLBACK_KEY: u64 = 0x9e37_79b9_7f4a_7c15;

/// Builds a render pipeline. NVN writes the point size of every draw; Metal refuses a point size write for a line or
/// triangle class, so such a pipeline is built again without a class, and the key is remembered in `fallbacks`.
fn build_render(
    cache: &PipelineCache,
    fallbacks: &Mutex<FastSet<u64>>,
    key: u64,
    desc: &mut RenderPipelineDesc<'_>,
) -> Result<(RenderPipeline, Source), nxkit_metal::MetalError> {
    if fallbacks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&key)
    {
        desc.topology = TopologyClass::Unspecified;
        return cache.render_traced(key ^ FALLBACK_KEY, desc);
    }
    cache.render_traced(key, desc).or_else(|e| {
        if desc.topology == TopologyClass::Point || !e.to_string().contains("point size") {
            return Err(e);
        }
        desc.topology = TopologyClass::Unspecified;
        let pipeline = cache.render_traced(key ^ FALLBACK_KEY, desc)?;
        // Remember the successful variant. Retrying the rejected descriptor for every draw invokes the Metal compiler
        // thousands of times per frame even though the fallback itself is cached.
        fallbacks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key);
        Ok(pipeline)
    })
}

impl Builds {
    /// Queues `job` once per key. The threads start with the first job.
    fn start(
        self: &Arc<Self>,
        cache: &Arc<PipelineCache>,
        fallbacks: &Arc<Mutex<FastSet<u64>>>,
        job: Job,
    ) {
        if !self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job.key)
        {
            return;
        }
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<Job>();
            let rx = Arc::new(Mutex::new(rx));
            for i in 0..COMPILE_THREADS {
                let (rx, me, cache, fallbacks) =
                    (rx.clone(), self.clone(), cache.clone(), fallbacks.clone());
                let _ = std::thread::Builder::new()
                    .name(format!("nxkit-pipeline-{i}"))
                    .spawn(move || {
                        nxkit_metal::thread::set_utility();
                        loop {
                            let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                            let Ok(job) = next else { return };
                            me.run(&cache, &fallbacks, &job);
                        }
                    });
            }
            *queue = Some(tx);
        }
        if let Some(tx) = queue.as_ref()
            && let Err(e) = tx.send(job)
        {
            self.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&e.0.key);
        }
    }

    fn run(&self, cache: &PipelineCache, fallbacks: &Mutex<FastSet<u64>>, job: &Job) {
        let built = match &job.kind {
            JobKind::Render(r) => {
                let mut desc = RenderPipelineDesc::new(
                    (&r.vertex.0, r.vertex.1.as_str()),
                    r.fragment.as_ref().map(|(l, e)| (l, e.as_str())),
                    &[],
                );
                desc.colors = r.colors.clone();
                desc.vertex_layout = r.vertex_layout.clone();
                desc.topology = r.topology;
                desc.sample_count = r.sample_count;
                desc.alpha_to_coverage = r.alpha_to_coverage;
                build_render(cache, fallbacks, job.key, &mut desc).map(|(_, source)| {
                    crate::coverage::record(cache, source, Some(&desc), &job.what);
                })
            }
            JobKind::Mesh(m) => cache.mesh_traced(job.key, &m.desc()).map(|(_, source)| {
                crate::coverage::record(cache, source, None, &job.what);
            }),
        };
        if let Err(e) = built {
            pipe_failed(format_args!("{}: {e}", job.what));
            self.failed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(job.key);
        }
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&job.key);
    }
}

impl Pipelines {
    pub(crate) fn new(
        ctx: &Context,
        archive: &ArchiveConfig,
    ) -> Result<Self, nxkit_metal::MetalError> {
        Ok(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            ctx: ctx.clone(),
            cache: Arc::new(PipelineCache::new(ctx, archive)?),
            depth: Mutex::new(FastMap::default()),
            flat: Mutex::new(None),
            paint: Mutex::new(None),
            host: Mutex::new(FastMap::default()),
            topology_fallbacks: Arc::new(Mutex::new(FastSet::default())),
            builds: Arc::new(Builds::default()),
        })
    }

    /// The cache that the deswizzle kernels and compute pipelines share.
    pub(crate) fn cache(&self) -> &PipelineCache {
        &self.cache
    }

    /// The number of pipelines made and found so far.
    pub(crate) fn counts(&self) -> (u64, u64) {
        (self.cache.builds(), self.cache.hits())
    }

    /// The compute pipeline of one of the host's own kernels: `entry` of the MSL `source`, compiled once under `name`.
    pub(crate) fn host_compute(
        &self,
        name: &'static str,
        source: &str,
        entry: &str,
    ) -> Result<ComputePipeline, Skip> {
        let lib = {
            let mut libs = self.host.lock().unwrap_or_else(PoisonError::into_inner);
            match libs.get(name) {
                Some(l) => l.clone(),
                None => {
                    let l = ShaderLibrary::from_source(&self.ctx, name, source)
                        .map_err(|e| pipe_failed(format_args!("host kernel {name}: {e}")))?;
                    libs.insert(name, l.clone());
                    l
                }
            }
        };
        let mut h = DefaultHasher::new();
        ("host", name, entry).hash(&mut h);
        self.cache
            .compute(h.finish(), &lib, entry)
            .map_err(|e| pipe_failed(format_args!("host kernel {name}: {e}")))
    }

    /// The compute pipeline of a program.
    pub(crate) fn compute(&self, key: &str, p: &StageProgram) -> Result<ComputePipeline, Skip> {
        let mut h = DefaultHasher::new();
        ("compute", key).hash(&mut h);
        self.cache
            .compute(h.finish(), &p.lib, &p.entry)
            .map_err(|e| pipe_failed(format_args!("compute {}: {e}", p.entry)))
    }

    /// The depth and stencil state object of a description, made once.
    pub(crate) fn depth_stencil(&self, d: &DepthStencilDesc) -> Result<DepthStencilState, Skip> {
        let hit = LAST_DEPTH.with(|l| {
            l.borrow()
                .as_ref()
                .filter(|m| m.owner == self.id && m.desc == *d)
                .map(|m| m.state.clone())
        });
        if let Some(s) = hit {
            return Ok(s);
        }
        let state = self.depth_stencil_uncached(d)?;
        LAST_DEPTH.with(|l| {
            *l.borrow_mut() = Some(LastDepth {
                owner: self.id,
                desc: *d,
                state: state.clone(),
            })
        });
        Ok(state)
    }

    fn depth_stencil_uncached(&self, d: &DepthStencilDesc) -> Result<DepthStencilState, Skip> {
        let mut map = self.depth.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = map.get(d) {
            return Ok(s.clone());
        }
        let s = DepthStencilState::new(&self.ctx, d)
            .map_err(|e| pipe_failed(format_args!("depth-stencil {d:?}: {e}")))?;
        map.insert(*d, s.clone());
        Ok(s)
    }
}

/// The colour targets of a pipeline: the formats of the pass with the blend state and write mask of the draw. A target
/// that the fragment shader does not write, or every target without a fragment shader, keeps its contents.
fn color_targets(
    d: &DrawDetail,
    fs: Option<&StageProgram>,
    targets: &Targets,
) -> Vec<Option<nxkit_metal::ColorTarget>> {
    targets
        .colors
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let format = (*f)?;
            let written =
                fs.is_some_and(|f| f.colors.as_ref().is_none_or(|c| c.contains(&(i as u8))));
            let blend = d.blend.get(i).copied().unwrap_or_default();
            let mask = d.write_masks.get(i).copied().unwrap_or(0xF);
            Some(translate::color_target(format, &blend, mask, written))
        })
        .collect()
}

fn fragment_entry<'a>(fs: &'a StageProgram, targets: &Targets) -> &'a str {
    let mut present = 0u8;
    let mut integer = 0u8;
    for (slot, format) in targets.colors.iter().enumerate() {
        let Some(format) = format else { continue };
        if fs
            .colors
            .as_ref()
            .is_some_and(|colors| !colors.contains(&(slot as u8)))
        {
            continue;
        }
        present |= 1 << slot;
        if crate::format_map::is_integer(*format) {
            integer |= 1 << slot;
        }
    }
    if integer != 0 {
        if integer == present {
            if let Some(entry) = &fs.entry_uint {
                return entry;
            }
        } else if let Some((entry, mask)) = &fs.entry_mixed
            && mask & present == integer
        {
            return entry;
        }
    }
    &fs.entry
}
