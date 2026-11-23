//! The encoding of the captures of one render pass (see the module documentation of [`super`]).

use std::sync::Arc;

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputeCommand, ComputePipeline, Dependency, Primitive,
    RenderCommand, RenderPassDesc, RenderPipeline, Scope, Scratch, Stages, Visibility,
};

use crate::bindless::{ScratchSink, StageInputs, fill_args};
use crate::capture_facts::Geometry;
use crate::detail::{DrawArgs, DrawDetail, Stage};
use crate::drawctx::Shared;
use crate::pipelines::Targets;
use crate::programs::StageProgram;
use crate::skip::Skip;
use crate::stats::EncodeStats;
use crate::translate;

/// The host kernels, compiled once (`Pipelines::host_compute`).
pub(super) const MSL: &str = include_str!("kernels.metal");
/// The bytes of `struct job` of the kernels.
const JOB_BYTES: usize = 176;
/// The bytes of `gs_capture` and of `tf_capture`.
const GS_BYTES: usize = 336;
const TF_BYTES: usize = 64;
/// The bytes of `vb_info_t`.
const VB_INFO_BYTES: usize = 32 + 2 * 32 * 16;
/// The words of a captured vertex for a full geometry kernel.
const CAPTURE_WORDS: u32 = 144;
/// The buffer slot of the vertex capture block (`main0_gs`), of the first feedback buffer and of the capture block of
/// `main0_tf` (`nxb-xlate` `msl::abi`).
const GS_SLOT: u32 = 13;
const TF_SLOT: u32 = 8;
const TF_STATE_SLOT: u32 = 14;
/// The most vertices one capture takes, and the bound of an indirect count without a stream to bound it.
const MAX_VERTICES: u64 = 1 << 16;
const DEFAULT_VERTICES: u64 = 4096;

/// A capture whose draw the encoder left out of its render pass: the ordinal of the draw.
type Capture = u32;

/// What the geometry stage of a capture needs.
struct Kernel {
    program: Arc<StageProgram>,
    pipeline: ComputePipeline,
    args: u64,
    dispatch: u64,
    /// The vertex capture: its pipeline and its block.
    capture: RenderPipeline,
    block: u64,
}

/// One capture, ready to encode.
struct Job {
    job: u64,
    draw_vs: u64,
    draw_tf: u64,
    tf: u64,
    vertex_args: u64,
    streams: Vec<(u32, u64)>,
    primitive: Primitive,
    kernel: Option<Kernel>,
    /// The pipeline of the capture entry and its argument buffer (of the vertex shader or of the reader `_bgv`).
    tf_pipeline: RenderPipeline,
    tf_args: u64,
    tf_primitive: Primitive,
    buffers: [u64; 4],
}

/// Runs the captures `draws` of a render pass, in their order, before the pass. A capture that cannot run is counted
/// as `skip.feedback`, and its buffers keep what they held.
pub(crate) fn run(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    (vertex, fragment): (&ArgumentTable, &ArgumentTable),
    scratch: &Scratch,
    draws: &[Capture],
    stats: &mut EncodeStats,
) {
    let (Ok(setup), Ok(finish)) = (
        sh.pipelines.host_compute("nxkit-feedback", MSL, "fb_setup"),
        sh.pipelines
            .host_compute("nxkit-feedback", MSL, "fb_finish"),
    ) else {
        for _ in draws {
            stats.skip(Skip::Pipeline);
        }
        return;
    };
    let mut jobs = Vec::with_capacity(draws.len());
    for &n in draws {
        let Some(d) = sh.detail.draws.get(n as usize) else {
            continue;
        };
        match job(sh, scratch, d) {
            Ok(j) => jobs.push(j),
            Err(why) => {
                stats.skip_draw(Some(&d.args), why);
                note(d, why);
            }
        }
    }
    if jobs.is_empty() {
        return;
    }
    let all = Dependency {
        scope: Scope::FromEarlierPasses,
        after: Stages::ALL,
        before: Stages::ALL,
        visibility: Visibility::Device,
    };
    let enc_ok = |r: nxkit_metal::Result<()>, stats: &mut EncodeStats| {
        if r.is_err() {
            stats.skip(Skip::Encode);
        }
    };
    // 1: the counts of the draws.
    if let Ok(mut cp) = cb.begin_compute_pass(vertex) {
        stats.compute_passes += 1;
        enc_ok(cp.encode(&ComputeCommand::Depend(all)), stats);
        for j in &jobs {
            for c in [
                ComputeCommand::SetPipeline(setup.clone()),
                ComputeCommand::SetBuffer {
                    index: 0,
                    gpu_addr: j.job,
                },
                ComputeCommand::DispatchThreadgroups {
                    groups: [1, 1, 1],
                    threads: [1, 1, 1],
                },
            ] {
                enc_ok(cp.encode(&c), stats);
            }
        }
        cp.end();
    }
    // 2: the vertex captures of the geometry shaders.
    if jobs.iter().any(|j| j.kernel.is_some()) {
        let desc = RenderPassDesc::default();
        if let Ok(mut rp) = cb.begin_render_pass_split(&desc, vertex, fragment) {
            stats.render_passes += 1;
            enc_ok(rp.encode(&RenderCommand::Depend(all)), stats);
            for j in &jobs {
                let Some(k) = &j.kernel else { continue };
                let mut cmds = vec![
                    RenderCommand::SetPipeline(k.capture.clone()),
                    RenderCommand::SetBuffer {
                        index: 0,
                        gpu_addr: j.vertex_args,
                    },
                    RenderCommand::SetBuffer {
                        index: GS_SLOT,
                        gpu_addr: k.block,
                    },
                ];
                cmds.extend(streams(&j.streams));
                cmds.push(RenderCommand::DrawIndirect {
                    primitive: j.primitive,
                    indirect_addr: j.draw_vs,
                });
                for c in &cmds {
                    enc_ok(rp.encode(c), stats);
                }
            }
            rp.end();
        }
    }
    // 3: the geometry kernels, the state steps and the records.
    if let Ok(mut cp) = cb.begin_compute_pass(vertex) {
        stats.compute_passes += 1;
        enc_ok(cp.encode(&ComputeCommand::Depend(all)), stats);
        for j in &jobs {
            if let Some(k) = &j.kernel {
                for c in [
                    ComputeCommand::SetPipeline(k.pipeline.clone()),
                    ComputeCommand::SetBuffer {
                        index: 0,
                        gpu_addr: k.args,
                    },
                    ComputeCommand::DispatchThreadgroupsIndirect {
                        gpu_addr: k.dispatch,
                        threads: k.program.threadgroup,
                    },
                    ComputeCommand::Barrier,
                ] {
                    enc_ok(cp.encode(&c), stats);
                }
            }
            for c in [
                ComputeCommand::SetPipeline(finish.clone()),
                ComputeCommand::SetBuffer {
                    index: 0,
                    gpu_addr: j.job,
                },
                ComputeCommand::DispatchThreadgroups {
                    groups: [1, 1, 1],
                    threads: [1, 1, 1],
                },
                ComputeCommand::Barrier,
            ] {
                enc_ok(cp.encode(&c), stats);
            }
        }
        cp.end();
    }
    // 4: the records into the feedback buffers. The passes after it read them.
    let desc = RenderPassDesc::default();
    if let Ok(mut rp) = cb.begin_render_pass_split(&desc, vertex, fragment) {
        stats.render_passes += 1;
        enc_ok(rp.encode(&RenderCommand::Depend(all)), stats);
        for j in &jobs {
            let mut cmds = vec![
                RenderCommand::SetPipeline(j.tf_pipeline.clone()),
                RenderCommand::SetBuffer {
                    index: 0,
                    gpu_addr: j.tf_args,
                },
                RenderCommand::SetBuffer {
                    index: TF_STATE_SLOT,
                    gpu_addr: j.tf,
                },
            ];
            for (b, &a) in j.buffers.iter().enumerate() {
                if a != 0 {
                    cmds.push(RenderCommand::SetBuffer {
                        index: TF_SLOT + b as u32,
                        gpu_addr: a,
                    });
                }
            }
            if j.kernel.is_none() {
                cmds.extend(streams(&j.streams));
            }
            cmds.push(RenderCommand::DrawIndirect {
                primitive: j.tf_primitive,
                indirect_addr: j.draw_tf,
            });
            for c in &cmds {
                enc_ok(rp.encode(c), stats);
            }
        }
        let later = Dependency {
            scope: Scope::ToLaterPasses,
            after: Stages::ALL,
            before: Stages::ALL,
            visibility: Visibility::Device,
        };
        enc_ok(rp.encode(&RenderCommand::Depend(later)), stats);
        rp.end();
    }
    stats.kinds.captures += jobs.len() as u32;
}

fn streams(s: &[(u32, u64)]) -> impl Iterator<Item = RenderCommand> + '_ {
    s.iter()
        .map(|&(index, gpu_addr)| RenderCommand::SetBuffer { index, gpu_addr })
}

/// Counts and shows why a capture did not run.
fn note(d: &DrawDetail, why: Skip) {
    nxkit_nvn::diag::example(
        nxkit_nvn::diag::Reason::SkipFeedback,
        format_args!(
            "capture of vs {} gs {:?}: {why:?}",
            d.program.vertex,
            d.capture.as_ref().and_then(|c| c.geometry.as_deref())
        ),
    );
}

/// The vertex program `p` with `entry` in the place of its discard entry: `Pipelines::render` builds the pipeline of
/// that entry without rasterization.
fn with_entry(p: &StageProgram, entry: &str) -> StageProgram {
    let mut p = p.clone();
    p.entry_discard = Some(entry.to_owned());
    p
}

/// The draw `d` as the pipeline of a capture entry sees it: no fragment stage, rasterizer discard.
fn capture_draw(d: &DrawDetail, vertex: Option<Arc<str>>, topology: Option<u32>) -> DrawDetail {
    let mut c = d.clone();
    c.raster.discard = true;
    c.program.fragment = None;
    c.program.geometry = None;
    c.capture = None;
    if let Some(v) = vertex {
        c.program.vertex = v;
        c.attributes.clear();
        c.streams.clear();
    }
    if let Some(t) = topology {
        c.topology = t;
    }
    c
}

/// Puts `bytes` into the scratch buffer.
fn put(scratch: &Scratch, bytes: &[u8]) -> Result<u64, Skip> {
    scratch
        .push(bytes)
        .map_err(|e| crate::buf_skip(format_args!("capture {} bytes: {e:?}", bytes.len())))
}

fn zeros(scratch: &Scratch, n: u64) -> Result<u64, Skip> {
    put(
        scratch,
        &vec![0u8; usize::try_from(n.max(16)).map_err(|_| Skip::Buffer)?],
    )
}

/// The real address of a guest range, and its bytes up to `size`. 0 for no range.
fn real(sh: &Shared<'_>, addr: u64, size: u64, class: u8) -> Result<(u64, u64), Skip> {
    if addr == 0 {
        return Ok((0, 0));
    }
    let loc = sh
        .pools
        .resolve_start_c(addr, class)
        .map_err(|_| Skip::Buffer)?;
    Ok((loc.real, size.min(loc.room)))
}

/// The vertices the capture of `d` holds at most: its own count, or what its streams can feed.
fn vertex_bound(d: &DrawDetail) -> u64 {
    let n = match d.args {
        DrawArgs::Arrays {
            count, instances, ..
        } => u64::from(count) * u64::from(instances.max(1)),
        _ => d
            .streams
            .iter()
            .filter(|s| s.divisor == 0 && s.stride != 0)
            .map(|s| s.size / u64::from(s.stride))
            .max()
            .unwrap_or(DEFAULT_VERTICES),
    };
    n.clamp(1, MAX_VERTICES)
}

/// The bytes of `struct job` of the kernels: its twelve addresses, the record strides and buffer capacities, then its
/// twelve words.
pub(super) fn job_bytes(
    addrs: [u64; 12],
    strides: [u32; 4],
    capacity: [u32; 4],
    words: [u32; 12],
) -> Vec<u8> {
    let mut w = Vec::with_capacity(JOB_BYTES);
    for p in addrs {
        w.extend_from_slice(&p.to_le_bytes());
    }
    for v in strides.iter().chain(&capacity).chain(&words) {
        w.extend_from_slice(&v.to_le_bytes());
    }
    debug_assert_eq!(w.len(), JOB_BYTES);
    w
}

/// Prepares the capture of `d`: the scratch blocks, the argument buffers and the pipelines.
fn job(sh: &Shared<'_>, scratch: &Scratch, d: &DrawDetail) -> Result<Job, Skip> {
    let c = d.capture.as_deref().ok_or(Skip::Detail)?;
    if !super::runnable(&d.args, d.topology) {
        return Err(Skip::Primitive);
    }
    // The capture entries are in `_b`, which reads its vertices through stage_in.
    let vs = sh.programs.library(&d.program.vertex, "_b", true)?;
    if !crate::draws::stage_in_fits(d, &vs) {
        return Err(Skip::Format);
    }
    let (primitive, _) = translate::primitive(d.topology).ok_or(Skip::Primitive)?;
    let vpp: u32 = match d.topology {
        1 => 2,
        4 => 3,
        _ => 1,
    };
    let targets = Targets {
        colors: Vec::new(),
        samples: 1,
    };
    let mut sink = ScratchSink::new(scratch, sh.uploaded, None).with_preloaded(sh.preloaded);
    let base_instance = match d.args {
        DrawArgs::Arrays { base_instance, .. } => base_instance,
        _ => 0,
    };
    let cap = vertex_bound(d);
    // The geometry shader: its kernel for the input topology and its reader.
    let gs = match &c.geometry {
        Some(g) => {
            let t = super::kernel_topology(d.topology).ok_or(Skip::Primitive)?;
            let kernel = sh.programs.library(g, &format!("_bg{t}"), false)?;
            let reader = sh.programs.library(g, "_bgv", true)?;
            let geo: Geometry = kernel.capture.geometry.ok_or(Skip::Program)?;
            Some((kernel, reader, geo))
        }
        None => None,
    };
    let strides = match &gs {
        Some((_, reader, _)) => reader.capture.tf_strides,
        None => vs.capture.tf_strides,
    };
    let mut buffers = [0u64; 4];
    let mut capacity = [0u32; 4];
    for b in 0..4 {
        if strides[b] == 0 {
            continue;
        }
        let (addr, size) = c.buffers[b];
        let (r, room) = real(sh, addr, size, crate::pools::overlay::SBUF)?;
        buffers[b] = r;
        capacity[b] = u32::try_from(room).unwrap_or(u32::MAX);
    }
    let inputs = |stage, geometry| StageInputs {
        stage,
        ubos: &d.ubos,
        sbufs: &d.sbufs,
        textures: &d.textures,
        samplers: &d.samplers,
        images: &d.images,
        base_instance,
        groups: None,
        push: d.push.as_deref(),
        blendz: 0,
        geometry,
    };
    let vertex_args = fill_args(&vs, &inputs(Stage::Vertex, None), &sh.env, &mut sink)?;
    let mut streams = Vec::with_capacity(d.streams.len() + 1);
    for s in &d.streams {
        let loc = crate::geometry::stream_loc(sh.pools, s)?;
        streams.push((translate::stream_slot(s.index), loc.real));
    }
    if translate::needs_default_vertex(d, &vs.inputs) {
        streams.push((translate::DEFAULT_VERTEX_SLOT, put(scratch, &[0; 16])?));
    }
    let draw_vs = zeros(scratch, 16)?;
    let draw_tf = zeros(scratch, 16)?;
    let tf = zeros(scratch, TF_BYTES as u64)?;
    let state = zeros(scratch, 32)?;
    let (src, _) = match d.args {
        DrawArgs::ArraysIndirect { indirect } => {
            real(sh, indirect, 16, crate::pools::overlay::COPY)?
        }
        _ => (0, 0),
    };
    let (load, _) = real(sh, c.load.unwrap_or(0), 32, crate::pools::overlay::COPY)?;
    let (save, _) = real(sh, c.save.unwrap_or(0), 32, crate::pools::overlay::COPY)?;
    let mut kernel = None;
    let mut ib = 0;
    let mut order = 0;
    let mut gs_block = 0;
    let mut vb_info = 0;
    let mut dispatch = 0;
    let (tf_pipeline, tf_args, tf_primitive) = match &gs {
        Some((kprog, reader, geo)) => {
            let entry_gs = vs.capture.entry_gs.as_deref().ok_or(Skip::Program)?;
            let entry_tf = reader.capture.entry_tf.as_deref().ok_or(Skip::Program)?;
            let lists = (cap / u64::from(vpp)) * u64::from(geo.invocations);
            let captured = zeros(scratch, cap * u64::from(CAPTURE_WORDS) * 4)?;
            let gvb = zeros(
                scratch,
                lists * u64::from(geo.vertices) * u64::from(geo.stride) * 4,
            )?;
            ib = zeros(scratch, lists * u64::from(geo.index_stride) * 4)?;
            order = zeros(scratch, lists * u64::from(geo.index_stride) * 3 * 4)?;
            vb_info = zeros(scratch, VB_INFO_BYTES as u64)?;
            dispatch = zeros(scratch, 16)?;
            // The topology table of a list: primitive `p` has the vertices `vpp * p` on.
            let trb: Vec<u8> = (0..cap as u32).flat_map(u32::to_le_bytes).collect();
            let trb = put(scratch, &trb)?;
            let mut block = vec![0u8; GS_BYTES];
            block[0..8].copy_from_slice(&captured.to_le_bytes());
            block[32..36].copy_from_slice(&CAPTURE_WORDS.to_le_bytes());
            for k in 0..CAPTURE_WORDS as usize {
                block[36 + 2 * k..38 + 2 * k].copy_from_slice(&(k as i16).to_le_bytes());
            }
            gs_block = put(scratch, &block)?;
            let helpers = Some([captured, gvb, ib, vb_info, trb]);
            let kargs = fill_args(kprog, &inputs(Stage::Geometry, helpers), &sh.env, &mut sink)?;
            let rargs = fill_args(reader, &inputs(Stage::Vertex, helpers), &sh.env, &mut sink)?;
            let g = c.geometry.clone().ok_or(Skip::Program)?;
            let t = super::kernel_topology(d.topology).ok_or(Skip::Primitive)?;
            let pipeline = sh.pipelines.compute(&format!("{g}_bg{t}"), kprog)?;
            let (capture, _) = sh.pipelines.render(
                &capture_draw(d, None, None),
                &with_entry(&vs, entry_gs),
                None,
                &targets,
                false,
            )?;
            let reader_key: Arc<str> = Arc::from(format!("{g}_bgv"));
            let (tfp, _) = sh.pipelines.render(
                &capture_draw(d, Some(reader_key), Some(0)),
                &with_entry(reader, entry_tf),
                None,
                &targets,
                false,
            )?;
            kernel = Some(Kernel {
                program: kprog.clone(),
                pipeline,
                args: kargs,
                dispatch,
                capture,
                block: gs_block,
            });
            (tfp, rargs, Primitive::Point)
        }
        None => {
            let entry_tf = vs.capture.entry_tf.as_deref().ok_or(Skip::Program)?;
            let (tfp, _) = sh.pipelines.render(
                &capture_draw(d, None, None),
                &with_entry(&vs, entry_tf),
                None,
                &targets,
                false,
            )?;
            (tfp, vertex_args, primitive)
        }
    };
    let geo = gs.as_ref().map(|g| g.2).unwrap_or_default();
    let (count, first, instances) = match d.args {
        DrawArgs::Arrays {
            first,
            count,
            instances,
            ..
        } => (count, first, instances),
        _ => (0, 0, 1),
    };
    let w = job_bytes(
        [
            state, load, src, save, gs_block, vb_info, draw_vs, dispatch, ib, order, tf, draw_tf,
        ],
        strides,
        capacity,
        [
            u32::from(c.begin),
            count,
            first,
            instances,
            base_instance,
            cap as u32,
            vpp,
            geo.invocations,
            geo.index_stride,
            geo.output,
            gs.as_ref().map_or(1, |g| g.0.threadgroup[0].max(1)),
            0,
        ],
    );
    let job = put(scratch, &w)?;
    Ok(Job {
        job,
        draw_vs,
        draw_tf,
        tf,
        vertex_args,
        streams,
        primitive,
        kernel,
        tf_pipeline,
        tf_args,
        tf_primitive,
        buffers,
    })
}
