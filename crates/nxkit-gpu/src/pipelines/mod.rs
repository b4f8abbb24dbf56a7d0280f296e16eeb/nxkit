//! Pipelines keyed by their translated state (R1, R17).
//!
//! The key is a hash of everything that goes into the Metal pipeline descriptor: the two shader keys, the colour
//! targets with their blend state and write mask, the vertex layout (none when the vertex function pulls its inputs,
//! `crate::pull`), the topology class, the sample count and the alpha-to-coverage flag. Two draws with the same
//! translated state share one pipeline, however different their NVN state was. The depth and stencil state is a
//! separate object that Metal binds per draw and not per pipeline.

mod builds;
mod prewarm;
mod record;
mod render;
mod runtime;
mod stats;
#[cfg(test)]
mod tests;

pub use runtime::flush_runtime_cache;
pub(crate) use runtime::plan as runtime_plan;
pub(crate) use stats::frame_done;

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

use builds::{Builds, Job, JobKind, MeshJob, RenderJob, async_on};

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
    /// The vertex function pulls: the attributes and streams are not part of the pipeline and stay empty.
    pull: bool,
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
            && (self.pull
                || self.attributes == d.attributes
                    && self.streams.len() == d.streams.len()
                    && self
                        .streams
                        .iter()
                        .zip(&d.streams)
                        .all(|(m, s)| *m == (s.index, s.stride, s.divisor)))
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

impl Drop for Pipelines {
    fn drop(&mut self) {
        self.builds.close();
    }
}

/// Whether the pipeline cache compiles the unspecialized pipeline of a base that the archive lacks and specializes it
/// per colour state (`ArchiveConfig::compile_bases`): off unless `NXKIT_PIPELINE_BASES=1`. In Ocarina of Time every
/// base was as rare as its pipelines (the vertex layout is part of it), an unspecialized compile took 0.1 to 10 s
/// where the full compile took 1 to 6 ms, and text of the title screen went missing.
pub(crate) fn bases_on() -> bool {
    std::env::var("NXKIT_PIPELINE_BASES").is_ok_and(|v| v == "1")
}

/// Whether the pipeline cache specializes the unspecialized pipelines of the archive (`ArchiveConfig::exact_only`
/// off): on unless `NXKIT_PIPELINE_SPECIALIZE=0`, which looks up and compiles exact pipelines only.
pub(crate) fn specialize_on() -> bool {
    std::env::var("NXKIT_PIPELINE_SPECIALIZE").map_or(true, |v| v != "0")
}

/// The point size fallback of [`build_render`] has this key.
const FALLBACK_KEY: u64 = 0x9e37_79b9_7f4a_7c15;

/// The key under which `fallbacks` remembers that the vertex function `entry` of the program `vertex` writes the
/// point size, so every later state of it skips the class at once.
fn point_size_key(vertex: &str, entry: &str) -> u64 {
    let mut h = DefaultHasher::new();
    ("point-size", vertex, entry).hash(&mut h);
    h.finish()
}

/// Whether the pipeline `key` of the vertex function `vertex` (its [`point_size_key`]) is built without a class.
fn falls_back(fallbacks: &Mutex<FastSet<u64>>, key: u64, vertex: u64) -> bool {
    let set = fallbacks.lock().unwrap_or_else(PoisonError::into_inner);
    set.contains(&key) || set.contains(&vertex)
}

/// Builds a render pipeline. NVN writes the point size of every draw; Metal refuses a point size write for a line or
/// triangle class, so such a pipeline is built again without a class. The key and the vertex function (`vertex`, its
/// [`point_size_key`]) are remembered in `fallbacks`: each refused compile costs as much as a good one, and a vertex
/// function used with many states paid one per state before.
fn build_render(
    cache: &PipelineCache,
    fallbacks: &Mutex<FastSet<u64>>,
    key: u64,
    vertex: u64,
    desc: &mut RenderPipelineDesc<'_>,
) -> Result<(RenderPipeline, Source), nxkit_metal::MetalError> {
    let built = build_render_uncounted(cache, fallbacks, key, vertex, desc);
    if let Ok((_, source)) = &built
        && record::armed()
    {
        let fb = desc.topology != TopologyClass::Point && falls_back(fallbacks, key, vertex);
        record::render(key, vertex, fb, desc, *source);
    }
    built
}

fn build_render_uncounted(
    cache: &PipelineCache,
    fallbacks: &Mutex<FastSet<u64>>,
    key: u64,
    vertex: u64,
    desc: &mut RenderPipelineDesc<'_>,
) -> Result<(RenderPipeline, Source), nxkit_metal::MetalError> {
    if desc.topology != TopologyClass::Point && falls_back(fallbacks, key, vertex) {
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
        let mut set = fallbacks.lock().unwrap_or_else(PoisonError::into_inner);
        set.insert(key);
        set.insert(vertex);
        Ok(pipeline)
    })
}

impl Pipelines {
    pub(crate) fn new(
        ctx: &Context,
        archive: &ArchiveConfig,
    ) -> Result<Self, nxkit_metal::MetalError> {
        let cache = Arc::new(PipelineCache::new(ctx, archive)?);
        stats::watch(&cache);
        Ok(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            ctx: ctx.clone(),
            cache,
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
        let key = h.finish();
        let made = self.cache.compute(key, &p.lib, &p.entry);
        if made.is_ok() {
            record::compute(key, &p.lib, &p.entry);
        }
        made.map_err(|e| pipe_failed(format_args!("compute {}: {e}", p.entry)))
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
