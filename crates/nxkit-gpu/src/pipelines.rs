//! Pipelines keyed by their translated state (R1, R17).
//!
//! The key is a hash of everything that goes into the Metal pipeline descriptor: the two shader keys, the colour
//! targets with their blend state and write mask, the vertex layout, the topology class, the sample count and the
//! alpha-to-coverage flag. Two draws with the same translated state share one pipeline, however different their NVN
//! state was. The depth and stencil state is a separate object that Metal binds per draw and not per pipeline.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{
    ArchiveConfig, ComputePipeline, Context, DepthStencilDesc, DepthStencilState, Format,
    PipelineCache, RenderPipeline, RenderPipelineDesc, ShaderLibrary, TopologyClass,
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
            && self.streams.iter().zip(&d.streams).all(|(m, s)| *m == (s.index, s.stride, s.divisor))
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

/// A render pipeline to compile in the background, with an owned copy of its description.
struct Job {
    key: u64,
    vertex: (ShaderLibrary, String),
    fragment: Option<(ShaderLibrary, String)>,
    colors: Vec<Option<nxkit_metal::ColorTarget>>,
    vertex_layout: nxkit_metal::VertexDescriptor,
    topology: TopologyClass,
    sample_count: u32,
    alpha_to_coverage: bool,
    what: String,
}

/// The threads that compile render pipelines in the background.
const COMPILE_THREADS: usize = 2;

/// Whether a render pipeline that the cache lacks compiles in the background (`NXKIT_PIPELINE_ASYNC=0` turns it off).
/// The draw is left out until it is ready, a frame or two, instead of stalling the encode for the 5 to 30 ms of the
/// compile.
fn async_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // The unit tests build synchronously, so a call returns its pipeline.
    *ON.get_or_init(|| !cfg!(test) && std::env::var("NXKIT_PIPELINE_ASYNC").map_or(true, |v| v != "0"))
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
) -> Result<RenderPipeline, nxkit_metal::MetalError> {
    if fallbacks.lock().unwrap_or_else(PoisonError::into_inner).contains(&key) {
        desc.topology = TopologyClass::Unspecified;
        return cache.render(key ^ FALLBACK_KEY, desc);
    }
    cache.render(key, desc).or_else(|e| {
        if desc.topology == TopologyClass::Point || !e.to_string().contains("point size") {
            return Err(e);
        }
        desc.topology = TopologyClass::Unspecified;
        let pipeline = cache.render(key ^ FALLBACK_KEY, desc)?;
        // Remember the successful variant. Retrying the rejected descriptor for every draw invokes the Metal compiler
        // thousands of times per frame even though the fallback itself is cached.
        fallbacks.lock().unwrap_or_else(PoisonError::into_inner).insert(key);
        Ok(pipeline)
    })
}

impl Builds {
    /// Queues `job` once per key. The threads start with the first job.
    fn start(self: &Arc<Self>, cache: &Arc<PipelineCache>, fallbacks: &Arc<Mutex<FastSet<u64>>>, job: Job) {
        if !self.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(job.key) {
            return;
        }
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<Job>();
            let rx = Arc::new(Mutex::new(rx));
            for i in 0..COMPILE_THREADS {
                let (rx, me, cache, fallbacks) = (rx.clone(), self.clone(), cache.clone(), fallbacks.clone());
                let _ = std::thread::Builder::new().name(format!("nxkit-pipeline-{i}")).spawn(move || {
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
            self.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&e.0.key);
        }
    }

    fn run(&self, cache: &PipelineCache, fallbacks: &Mutex<FastSet<u64>>, job: &Job) {
        let mut desc = RenderPipelineDesc::new(
            (&job.vertex.0, job.vertex.1.as_str()),
            job.fragment.as_ref().map(|(l, e)| (l, e.as_str())),
            &[],
        );
        desc.colors = job.colors.clone();
        desc.vertex_layout = job.vertex_layout.clone();
        desc.topology = job.topology;
        desc.sample_count = job.sample_count;
        desc.alpha_to_coverage = job.alpha_to_coverage;
        if let Err(e) = build_render(cache, fallbacks, job.key, &mut desc) {
            pipe_failed(format_args!("render {}: {e}", job.what));
            self.failed.lock().unwrap_or_else(PoisonError::into_inner).insert(job.key);
        }
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&job.key);
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

    /// The pipeline of a draw. `fs` is `None` for a draw without a fragment shader. `icb` asks for a pipeline that an
    /// indirect command buffer may use. A draw whose translated state equals that of the previous draw on the same
    /// thread gets the previous pipeline without a lookup.
    pub(crate) fn render(
        &self,
        d: &DrawDetail,
        vs: &StageProgram,
        fs: Option<&StageProgram>,
        targets: &Targets,
        icb: bool,
    ) -> Result<(RenderPipeline, u64), Skip> {
        let hit = LAST_PIPELINE.with(|l| {
            l.borrow().as_ref().filter(|m| m.matches(self.id, d, targets, icb)).map(|m| (m.pipeline.clone(), m.key))
        });
        if let Some(hit) = hit {
            return Ok(hit);
        }
        let (pipeline, key) = self.render_uncached(d, vs, fs, targets, icb)?;
        LAST_PIPELINE.with(|l| {
            *l.borrow_mut() = Some(Last {
                owner: self.id,
                icb,
                topology: d.topology,
                alpha_to_coverage: d.raster.alpha_to_coverage,
                program: d.program.clone(),
                targets: targets.clone(),
                blend: d.blend,
                write_masks: d.write_masks,
                attributes: d.attributes.clone(),
                streams: d.streams.iter().map(|s| (s.index, s.stride, s.divisor)).collect(),
                pipeline: pipeline.clone(),
                key,
            });
        });
        Ok((pipeline, key))
    }

    fn render_uncached(
        &self,
        d: &DrawDetail,
        vs: &StageProgram,
        fs: Option<&StageProgram>,
        targets: &Targets,
        icb: bool,
    ) -> Result<(RenderPipeline, u64), Skip> {
        let class = translate::primitive(d.topology).ok_or(Skip::Primitive)?.1;
        let flat = crate::knob("flat") && targets.colors.len() >= 4
            && targets.colors[..4].iter().all(Option::is_some)
            && fs.is_some_and(|f| f.colors.as_deref() == Some(&[0, 1, 2, 3]));
        let flat_lib = if flat {
            let mut library = self.flat.lock().unwrap_or_else(PoisonError::into_inner);
            if library.is_none() {
                *library = Some(ShaderLibrary::from_source(&self.ctx, "nxkit-flat-gbuffer", r#"
                    #include <metal_stdlib>
                    using namespace metal;
                    struct Out { float4 c0 [[color(0)]]; float4 c1 [[color(1)]];
                        float4 c2 [[color(2)]]; float4 c3 [[color(3)]]; };
                    fragment Out flat_gbuffer() { return { float4(0.5, 0.5, 1, 1), float4(1), float4(0.5), float4(0) }; }
                "#).map_err(|_| Skip::Pipeline)?);
            }
            library.clone()
        } else { None };
        let paint_dir = crate::knob("paintdir");
        let paint = (crate::knob("paint") || paint_dir)
            && targets.colors.len() == 1
            && targets.colors[0].is_some()
            && fs.is_some()
            && {
                let keys = crate::diag::paint_programs();
                keys.is_empty() || d.program.fragment.as_deref().is_some_and(|f| keys.iter().any(|k| k == f))
            };
        let paint_lib = if paint {
            let mut library = self.paint.lock().unwrap_or_else(PoisonError::into_inner);
            if library.is_none() {
                *library = Some(ShaderLibrary::from_source(&self.ctx, "nxkit-paint", r#"
                    #include <metal_stdlib>
                    using namespace metal;
                    struct In { float4 loc0 [[user(locn0)]]; };
                    fragment float4 paint_magenta() { return float4(8.0, 0.0, 8.0, 1.0); }
                    fragment float4 paint_dir(In in [[stage_in]]) { return float4(abs(in.loc0.xyz), 1.0); }
                "#).map_err(|_| Skip::Pipeline)?);
            }
            library.clone()
        } else { None };
        let mut desc = RenderPipelineDesc::new(
            (&vs.lib, vs.entry.as_str()),
            paint_lib
                .as_ref()
                .map(|lib| (lib, if paint_dir { "paint_dir" } else { "paint_magenta" }))
                .or_else(|| flat_lib.as_ref().map(|lib| (lib, "flat_gbuffer")))
                .or_else(|| fs.map(|f| (&f.lib, fragment_entry(f, targets)))),
            &[],
        );
        for (i, f) in targets.colors.iter().enumerate() {
            let Some(format) = *f else {
                desc.colors.push(None);
                continue;
            };
            let written =
                fs.is_some_and(|f| f.colors.as_ref().is_none_or(|c| c.contains(&(i as u8))));
            let blend = d.blend.get(i).copied().unwrap_or_default();
            let mask = d.write_masks.get(i).copied().unwrap_or(0xF);
            desc.colors
                .push(Some(translate::color_target(format, &blend, mask, written)));
        }
        desc.vertex_layout =
            translate::vertex_descriptor(d, &vs.inputs).map_err(|e| {
                nxkit_nvn::diag::example(
                    nxkit_nvn::diag::Reason::SkipFormat,
                    format_args!("vertex {e:?} vs {:?}", d.program.vertex),
                );
                Skip::Format
            })?;
        desc.topology = class;
        desc.sample_count = targets.samples;
        desc.alpha_to_coverage = d.raster.alpha_to_coverage;
        desc.support_icb = icb;
        let mut h = DefaultHasher::new();
        (
            &d.program.vertex,
            d.program.fragment.as_deref(),
            targets,
            icb,
        )
            .hash(&mut h);
        desc.colors.hash(&mut h);
        desc.vertex_layout.hash(&mut h);
        class.hash(&mut h);
        d.raster.alpha_to_coverage.hash(&mut h);
        flat.hash(&mut h);
        paint.hash(&mut h);
        paint_dir.hash(&mut h);
        let key = h.finish();
        if async_on() && !icb {
            let fallback = self.topology_fallbacks.lock().unwrap_or_else(PoisonError::into_inner).contains(&key);
            if let Some(p) = self.cache.cached_render(if fallback { key ^ FALLBACK_KEY } else { key }) {
                return Ok((p, key));
            }
            if self.builds.failed.lock().unwrap_or_else(PoisonError::into_inner).contains(&key) {
                return Err(Skip::Pipeline);
            }
            let job = Job {
                key,
                vertex: (desc.vertex.0.clone(), desc.vertex.1.to_string()),
                fragment: desc.fragment.map(|(l, e)| (l.clone(), e.to_string())),
                colors: desc.colors.clone(),
                vertex_layout: desc.vertex_layout.clone(),
                topology: desc.topology,
                sample_count: desc.sample_count,
                alpha_to_coverage: desc.alpha_to_coverage,
                what: format!("{} / {:?}", vs.entry, fs.map(|f| f.entry.as_str())),
            };
            self.builds.start(&self.cache, &self.topology_fallbacks, job);
            return Err(Skip::Pending);
        }
        let p = build_render(&self.cache, &self.topology_fallbacks, key, &mut desc).map_err(|e| {
            pipe_failed(format_args!("render {} / {:?}: {e}", vs.entry, fs.map(|f| f.entry.as_str())))
        })?;
        Ok((p, key))
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
            l.borrow().as_ref().filter(|m| m.owner == self.id && m.desc == *d).map(|m| m.state.clone())
        });
        if let Some(s) = hit {
            return Ok(s);
        }
        let state = self.depth_stencil_uncached(d)?;
        LAST_DEPTH.with(|l| *l.borrow_mut() = Some(LastDepth { owner: self.id, desc: *d, state: state.clone() }));
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

fn fragment_entry<'a>(fs: &'a StageProgram, targets: &Targets) -> &'a str {
    let mut present = 0u8;
    let mut integer = 0u8;
    for (slot, format) in targets.colors.iter().enumerate() {
        let Some(format) = format else { continue };
        if fs.colors.as_ref().is_some_and(|colors| !colors.contains(&(slot as u8))) { continue }
        present |= 1 << slot;
        if crate::format_map::is_integer(*format) { integer |= 1 << slot; }
    }
    if integer != 0 {
        if integer == present {
            if let Some(entry) = &fs.entry_uint { return entry }
        } else if let Some((entry, mask)) = &fs.entry_mixed {
            if mask & present == integer { return entry }
        }
    }
    &fs.entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::{DrawArgs, ProgramRef};

    #[test]
    fn a_rejected_topology_uses_the_cached_successful_variant() {
        let Ok(ctx) = Context::system_default() else { return };
        let lib = ShaderLibrary::from_source(&ctx, "point-size", r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; float size [[point_size]]; };
vertex V vs(uint id [[vertex_id]]) { return {float4(0,0,0,1), 1}; }
fragment float4 fs() { return float4(1); }
fragment uint4 fs_u() { return uint4(1); }
struct Mixed { uint4 integer [[color(0)]]; float4 color [[color(1)]]; };
fragment Mixed fs_m() { return {uint4(1), float4(1)}; }
"#).unwrap();
        let stage = |entry: &str| StageProgram { lib: lib.clone(), entry: entry.into(), entry_uint: None, entry_mixed: None, binds: vec![],
            arg_bytes: 0, inputs: vec![], colors: Some(vec![0]), threadgroup: [1;3], c1: vec![] };
        let vs = stage("vs");
        let mut fs = stage("fs");
        let draw = DrawDetail { program: ProgramRef { vertex: "point-size".into(), fragment: Some("fs".into()) },
            blend: Default::default(), write_masks: [15;8], depth_stencil: Default::default(),
            raster: Default::default(), topology: 4, attributes: vec![], streams: vec![],
            args: DrawArgs::Arrays { first: 0, count: 3, instances: 1, base_instance: 0 },
            ubos: vec![], sbufs: vec![], textures: vec![], dynamic: Default::default() };
        let targets = Targets { colors: vec![Some(Format::Rgba8Unorm)], samples: 1 };
        let pipelines = Pipelines::new(&ctx, &ArchiveConfig::default()).unwrap();
        let (_, key) = pipelines.render(&draw, &vs, Some(&fs), &targets, false).unwrap();
        assert!(pipelines.topology_fallbacks.lock().unwrap().contains(&key));
        for _ in 0..10 {
            assert_eq!(pipelines.render(&draw, &vs, Some(&fs), &targets, false).unwrap().1, key);
        }
        // The ten repeats are answered by the thread memo and never reach the cache.
        assert_eq!(pipelines.counts(), (1, 0));
        fs.entry_uint = Some("fs_u".into());
        let integer = Targets { colors: vec![Some(Format::Rgba16Uint)], samples: 1 };
        assert_eq!(fragment_entry(&fs, &integer), "fs_u");
        pipelines.render(&draw, &vs, Some(&fs), &integer, false).unwrap();
        fs.colors = Some(vec![0, 1]);
        fs.entry_mixed = Some(("fs_m".into(), 1));
        let mixed = Targets { colors: vec![Some(Format::Rgba16Uint), Some(Format::Rgba8Unorm)], samples: 1 };
        assert_eq!(fragment_entry(&fs, &mixed), "fs_m");
        pipelines.render(&draw, &vs, Some(&fs), &mixed, false).unwrap();
    }
}
