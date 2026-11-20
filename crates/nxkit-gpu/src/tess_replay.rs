//! No-hull draw replay for prevalidated domain libraries.
//!
//! Replay requires validated companion libraries and explicit patch state.
//! It supports bounded direct array and indexed draws, not indirect draws or guessed patch defaults.

use nxkit_metal::{
    Dependency, MetalError, Primitive, RenderCommand, RenderPass, RenderPipeline, Scope, Stages,
    Visibility,
};

use crate::bindless::{self, Sink, StageInputs};
use crate::detail::{DrawArgs, DrawDetail, Stage};
use crate::draws::{Geom, PassCtx, Prep, Shared, cull_of, fixed};
use crate::programs::{Bind, BindKind, StageProgram, tessellation::Link};
use crate::stats::Skip;

use crate::tess_pattern::{
    CAPTURE_WORDS, capture_descriptor, coordinate_records, index_descriptor,
};
const CAPTURE_SLOT: u32 = 13;
const COORD_SLOT: u32 = 27;
const STATE_SLOT: u32 = 28;
const DOMAIN_CAPTURE_SLOT: u32 = 26;
const INDEX_SLOT: u32 = 17;
const INDEX_INFO_SLOT: u32 = 18;
const INDEX_VALID_SLOT: u32 = 19;
const STREAM_LIMIT_SLOT: u32 = 20;
const DOMAIN_VALID_SLOT: u32 = 29;
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const MAX_PATTERN_BYTES: usize = 1024 * 1024;

pub(crate) struct Capture {
    pipeline: RenderPipeline,
    args: u64,
    streams: Vec<(u32, u64)>,
    descriptor: u64,
    first: u32,
    count: u32,
}

impl Capture {
    pub(crate) fn encode(&self, rp: &mut RenderPass<'_>) -> Result<(), MetalError> {
        // The index, vertex, or other capture input may have been written by earlier GPU work.
        // Make the resident bytes visible without a CPU snapshot. Also order writers
        // that ran earlier on this same render encoder.
        rp.encode(&RenderCommand::Depend(Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::ALL,
            before: Stages::VERTEX,
            visibility: Visibility::Device,
        }))?;
        rp.encode(&RenderCommand::Depend(Dependency {
            scope: Scope::WithinPass,
            after: Stages::VERTEX | Stages::FRAGMENT,
            before: Stages::VERTEX,
            visibility: Visibility::Device,
        }))?;
        rp.encode(&RenderCommand::SetPipeline(self.pipeline.clone()))?;
        for (index, gpu_addr) in [(0, self.args), (CAPTURE_SLOT, self.descriptor)]
            .into_iter()
            .chain(self.streams.iter().copied())
        {
            rp.encode(&RenderCommand::SetBuffer { index, gpu_addr })?;
        }
        rp.encode(&RenderCommand::Draw {
            primitive: Primitive::Point,
            first_vertex: self.first,
            vertex_count: self.count,
            instance_count: 1,
            base_instance: 0,
        })?;
        rp.encode(&RenderCommand::Depend(Dependency {
            scope: Scope::WithinPass,
            after: Stages::VERTEX,
            before: Stages::VERTEX,
            visibility: Visibility::Device,
        }))
    }
}

fn admits_binding(b: &Bind, stage: Stage) -> bool {
    match b.kind {
        BindKind::Img | BindKind::GmemMap | BindKind::GmemLog => false,
        BindKind::Sbuf => !b.rw,
        // The vertex driver block is existing and known. The domain stage does
        // not yet have an independently verified raw stage-specific driver ABI.
        BindKind::DrvCb if stage == Stage::TessEvaluation => b.bank == crate::bindless::PUSH_BANK,
        _ => true,
    }
}

fn admit_bindings(
    p: &StageProgram,
    stage: Stage,
    d: &DrawDetail,
    sh: &Shared<'_>,
) -> Result<(), Skip> {
    for b in &p.binds {
        if !admits_binding(b, stage) {
            return Err(Skip::Binding);
        }
        if stage != Stage::TessEvaluation {
            continue;
        }
        // Ordinary draws can substitute zero or default resources. This preview cannot
        // establish the correctness of doing so for an unbound evaluation-stage input.
        let has_buffer = |buffers: &[crate::detail::UboDetail], class| {
            buffers
                .iter()
                .find(|u| u.stage == stage && u.slot == b.reg)
                .is_some_and(|u| {
                    u.snapshot
                        .as_ref()
                        .is_some_and(|s| s.len() >= u.size as usize)
                        || sh
                            .pools
                            .resolve_c(u.address, u64::from(u.size), class)
                            .is_ok()
                })
        };
        let bound = match b.kind {
            BindKind::Cbuf | BindKind::UboPtr => has_buffer(&d.ubos, crate::overlay::UBO),
            BindKind::Sbuf => has_buffer(&d.sbufs, crate::overlay::SBUF),
            BindKind::Tex | BindKind::Samp | BindKind::SampTex => d
                .textures
                .iter()
                .find(|t| t.stage == stage && t.slot == b.reg)
                .and_then(|t| sh.env.handles.get(&t.handle))
                .is_some_and(|r| {
                    if b.kind == BindKind::Tex {
                        r.texture != 0
                    } else {
                        r.sampler != 0
                    }
                }),
            BindKind::DrvCb => d.push.is_some(),
            BindKind::C1 => !p.c1.is_empty(),
            _ => true,
        };
        if !bound {
            return Err(Skip::Binding);
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct IndexInput {
    address: u64,
    width: u32,
    base_vertex: i32,
}

fn check_indexed_attributes(
    attributes: &[crate::detail::AttributeDetail],
    lengths: &[u64],
    used: impl Fn(u32) -> bool,
) -> Result<(), Skip> {
    for a in attributes {
        if !used(a.location) || a.format == 0 {
            continue;
        }
        if a.location as usize >= crate::pull::ATTRIBUTES
            || lengths.get(a.stream as usize).is_none_or(|&n| n == 0)
            || crate::pull::attribute_word(a.format, a.offset, a.stream) == 0
        {
            return Err(Skip::Format);
        }
    }
    Ok(())
}

/// Builds the ordinary raw vertex pull layout plus a companion byte bound per stream.
/// Only fully resolved, four-byte aligned stream ranges enter the shader; its per-fetch
/// preflight must additionally check the index value, 64-bit offset and read footprint.
fn indexed_capture_streams(
    sh: &Shared<'_>,
    draw: &DrawDetail,
    sink: &mut dyn Sink,
    index: IndexInput,
    count: u32,
    vs: &StageProgram,
) -> Result<(Vec<(u32, u64)>, u64), Skip> {
    let index_bytes = u64::from(index.width) * u64::from(count);
    let src = sh
        .pools
        .resolve_c(index.address, index_bytes, crate::overlay::STREAM)
        .map_err(|_| Skip::Index)?;
    let mut streams = Vec::new();
    let mut lengths = [0; crate::pull::STREAMS];
    for s in &draw.streams {
        let j = s.index as usize;
        if j >= lengths.len() || lengths[j] != 0 || s.address & 3 != 0 || s.size == 0 {
            return Err(Skip::Buffer);
        }
        let loc = sh
            .pools
            .resolve_c(s.address, s.size, crate::overlay::STREAM)
            .map_err(|_| Skip::Buffer)?;
        if loc.real & 3 != 0 {
            return Err(Skip::Buffer);
        }
        streams.push((s.address, loc.real));
        lengths[j] = s.size;
    }
    check_indexed_attributes(&draw.attributes, &lengths, |location| {
        vs.inputs.iter().any(|(loc, _)| *loc == location)
    })?;
    let layout = crate::pull::layout(draw, |guest| {
        streams
            .iter()
            .find(|(g, _)| *g == guest)
            .map(|(_, real)| *real)
    });
    let length_bytes: Vec<u8> = lengths.into_iter().flat_map(u64::to_le_bytes).collect();
    let descriptor =
        index_descriptor(index.width, count, index.base_vertex, u32::MAX).ok_or(Skip::Index)?;
    let valid = sink.put(&vec![0; count as usize * 4])?;
    Ok((
        vec![
            (crate::pull::SLOT, sink.put(&layout)?),
            (INDEX_SLOT, src.real),
            (INDEX_INFO_SLOT, sink.put(&descriptor)?),
            (INDEX_VALID_SLOT, valid),
            (STREAM_LIMIT_SLOT, sink.put(&length_bytes)?),
        ],
        valid,
    ))
}

pub(crate) fn prepare(
    sh: &Shared<'_>,
    pc: &PassCtx,
    draw: &DrawDetail,
    sink: &mut dyn Sink,
) -> Result<Prep, Skip> {
    let p = draw.patch.as_ref().ok_or(Skip::Primitive)?;
    if !p.reflected || p.control.is_some() || !draw.images.is_empty() {
        return Err(Skip::Primitive);
    }
    let n = p
        .size
        .filter(|s| (1..=32).contains(s))
        .ok_or(Skip::Primitive)?;
    let inner = p.inner.ok_or(Skip::Primitive)?;
    let outer = p.outer.ok_or(Skip::Primitive)?;
    let (first, count, index) = match draw.args {
        DrawArgs::Arrays {
            first,
            count,
            instances: 1,
            base_instance: 0,
        } => (first, count, None),
        DrawArgs::Indexed {
            index_type,
            address,
            count,
            instances: 1,
            base_vertex,
            base_instance: 0,
        } if !p.restart && index_type <= 2 => (
            0,
            count,
            Some(IndexInput {
                address,
                width: 1 << index_type,
                base_vertex,
            }),
        ),
        _ => return Err(Skip::Primitive),
    };
    // Incomplete patches require a guest-specific rule. Refuse rather than truncate.
    if !count.is_multiple_of(n) || first.checked_add(count).is_none() {
        return Err(Skip::Primitive);
    }
    let pattern =
        crate::tess_pattern::pattern(inner.map(f32::from_bits), outer.map(f32::from_bits), true)
            .map_err(|_| Skip::Primitive)?;
    let expanded = (count / n)
        .checked_mul(pattern.len() as u32)
        .ok_or(Skip::Buffer)?;
    let vs = if index.is_some() {
        sh.programs.capture_indexed(&draw.program.vertex)?
    } else {
        sh.programs.capture(&draw.program.vertex)?
    };
    let domain_key = p.evaluation.as_ref().ok_or(Skip::Program)?;
    let ds = sh.programs.stage(domain_key, false)?;
    if !ds.inputs.is_empty() || index.is_some() && !(ds.domain_replay && ds.domain_replay_indexed) {
        return Err(Skip::Primitive);
    }
    admit_bindings(&vs, Stage::Vertex, draw, sh)?;
    admit_bindings(&ds, Stage::TessEvaluation, draw, sh)?;
    let fs = draw
        .program
        .fragment
        .as_ref()
        .map(|key| sh.programs.stage(key, false))
        .transpose()?;
    if index.is_none()
        && !ds.domain_replay
        && !Link::direct(vs.tess_link, ds.tess_link, fs.as_ref().map(|f| f.tess_link))
    {
        return Err(Skip::Program);
    }
    let cull = cull_of(draw)?;
    let f = fixed(sh, pc, draw, cull)?;
    // Clone only the small number of patch draws. Use the evaluator as the raster pipeline's vertex key,
    // with the original fragment and target state. A domain function does not use a vertex descriptor.
    let mut mapped = draw.clone();
    mapped.topology = 4;
    mapped.program.vertex = domain_key.clone();
    mapped.attributes.clear();
    mapped.streams.clear();
    let indexed_ds = index.map(|_| {
        let mut ds = (*ds).clone();
        ds.entry = "main0_tess_indexed_domain".to_owned();
        mapped.program.vertex = format!("{domain_key}:indexed-v1").into();
        ds
    });
    let (pipeline, key) = sh.pipelines.render_uncached(
        &mapped,
        indexed_ds.as_ref().unwrap_or(&ds),
        fs.as_deref(),
        &pc.targets,
        false,
    )?;

    let inputs = |stage| StageInputs {
        stage,
        ubos: &draw.ubos,
        sbufs: &draw.sbufs,
        textures: &draw.textures,
        images: &draw.images,
        base_instance: 0,
        groups: None,
        push: draw.push.as_deref(),
        blendz: match (&fs, stage) {
            (Some(f), Stage::Fragment) => crate::translate::blend_zero_word(draw, f, &pc.targets),
            _ => 0,
        },
    };
    let vertex_args = bindless::fill_args(&ds, &inputs(Stage::TessEvaluation), &sh.env, sink)?;
    let fragment_args = fs
        .as_ref()
        .map(|f| bindless::fill_args_scaled(f, &inputs(Stage::Fragment), &sh.env, pc.scale, sink))
        .transpose()?;

    let mut streams = Vec::new();
    let mut capture = None;
    if expanded != 0 {
        let capture_bytes = (count as usize)
            .checked_mul(CAPTURE_WORDS * 4)
            .filter(|s| *s <= MAX_CAPTURE_BYTES)
            .ok_or(Skip::Buffer)?;
        let coordinate_bytes = (expanded as usize)
            .checked_mul(32)
            .filter(|s| *s <= MAX_PATTERN_BYTES)
            .ok_or(Skip::Buffer)?;
        let addr = sink.put(&vec![0; capture_bytes])?;
        let coords =
            coordinate_records(&pattern, n, count / n, coordinate_bytes).ok_or(Skip::Buffer)?;
        let coords = sink.put(&coords)?;
        let mut state = Vec::with_capacity(48);
        for v in [
            n,
            u32::from(index.is_some()),
            if index.is_some() { count } else { 0 },
            0,
        ]
        .into_iter()
        .chain(outer)
        .chain(inner)
        .chain([0; 2])
        {
            state.extend(v.to_le_bytes());
        }
        streams.extend([
            (DOMAIN_CAPTURE_SLOT, addr),
            (COORD_SLOT, coords),
            (STATE_SLOT, sink.put(&state)?),
        ]);
        let mut capture_streams = Vec::new();
        if let Some(index) = index {
            let (args, validity) = indexed_capture_streams(sh, draw, sink, index, count, &vs)?;
            capture_streams = args;
            streams.push((DOMAIN_VALID_SLOT, validity));
        } else {
            for s in &draw.streams {
                let loc = crate::geometry::stream_loc(sh.pools, s)?;
                // Bound a conservatively aligned fetch for every input of the original shader.
                // A single instance always uses element zero of per-instance streams.
                let advance = if s.divisor == 0 {
                    u64::from(first + count - 1) * u64::from(s.stride)
                } else {
                    0
                };
                if let Some(offset) = draw
                    .attributes
                    .iter()
                    .filter(|a| {
                        a.stream == s.index
                            && vs
                                .inputs
                                .iter()
                                .any(|(location, _)| *location == a.location)
                    })
                    .map(|a| u64::from(a.offset))
                    .max()
                {
                    let end = advance.checked_add(offset + 16).ok_or(Skip::Buffer)?;
                    if end > loc.room.min(s.size) {
                        return Err(Skip::Buffer);
                    }
                }
                capture_streams.push((crate::translate::stream_slot(s.index), loc.real));
            }
            if crate::translate::needs_default_vertex(draw, &vs.inputs) {
                capture_streams.push((crate::translate::DEFAULT_VERTEX_SLOT, sink.put(&[0; 16])?));
            }
        }
        capture = Some(Capture {
            pipeline: sh.pipelines.vertex_capture(draw, &vs, &pc.targets)?,
            args: bindless::fill_args(&vs, &inputs(Stage::Vertex), &sh.env, sink)?,
            streams: capture_streams,
            descriptor: sink.put(&capture_descriptor(addr, first, count))?,
            first,
            count,
        });
    }
    Ok(Prep {
        capture,
        pipeline,
        key,
        depth_stencil: f.depth_stencil,
        depth_stencil_desc: f.depth_stencil_desc,
        vertex_args,
        fragment_args,
        object_args: None,
        stream_locs: vec![None; streams.len()],
        streams,
        pulled: false,
        pooled: bindless::binds_pool(&vs)
            || bindless::binds_pool(&ds)
            || fs.as_deref().is_some_and(bindless::binds_pool),
        cull: f.cull,
        winding: f.winding,
        fill: f.fill,
        bias: f.bias,
        clip: f.clip,
        geom: Geom::Arrays {
            primitive: Primitive::Triangle,
            first: 0,
            count: expanded,
            instances: 1,
            base_instance: 0,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(kind: BindKind, bank: u32, rw: bool) -> Bind {
        Bind {
            kind,
            bank,
            rw,
            arg_id: 0,
            reg: 0,
            dim: None,
            rscale: None,
            img: None,
        }
    }

    #[test]
    fn capture_and_domain_reject_memory_effects_and_unknown_driver_layout() {
        for kind in [BindKind::Img, BindKind::GmemLog, BindKind::GmemMap] {
            assert!(!admits_binding(&binding(kind, 0, false), Stage::Vertex));
            assert!(!admits_binding(
                &binding(kind, 0, false),
                Stage::TessEvaluation
            ));
        }
        assert!(!admits_binding(
            &binding(BindKind::Sbuf, 0, true),
            Stage::TessEvaluation
        ));
        assert!(admits_binding(
            &binding(BindKind::Sbuf, 0, false),
            Stage::TessEvaluation
        ));
        assert!(!admits_binding(
            &binding(BindKind::DrvCb, 0, false),
            Stage::TessEvaluation
        ));
        assert!(admits_binding(
            &binding(BindKind::DrvCb, 24, false),
            Stage::TessEvaluation
        ));
        assert!(admits_binding(
            &binding(BindKind::DrvCb, 0, false),
            Stage::Vertex
        ));
    }

    #[test]
    fn rejects_a_used_attribute_that_would_silently_become_zero() {
        let attr = |format, offset, stream| crate::detail::AttributeDetail {
            location: 0,
            format,
            offset,
            stream,
        };
        for a in [
            attr(1, 0x80000, 0),
            attr(0x100, 0, 0),
            attr(1, 0, 16),
            attr(1, 0, 1),
        ] {
            assert_eq!(
                check_indexed_attributes(&[a], &[128], |_| true),
                Err(Skip::Format)
            );
            assert!(check_indexed_attributes(&[a], &[128], |_| false).is_ok());
        }
        assert!(check_indexed_attributes(&[attr(0, 0, 0)], &[], |_| true).is_ok());
        assert!(check_indexed_attributes(&[attr(1, 0, 0)], &[128], |_| true).is_ok());
    }
}

#[cfg(test)]
#[path = "tess_replay_device.rs"]
mod device_tests;
