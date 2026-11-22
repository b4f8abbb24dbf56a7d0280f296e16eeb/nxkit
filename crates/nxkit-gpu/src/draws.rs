//! The translation of one NVN draw into the Metal objects and values that both encoding paths need.
//!
//! A draw goes into a render pass either command by command or into an indirect command buffer. Both paths call
//! [`prepare`]. It resolves the programs, builds the pipeline and the argument buffers and translates the fixed
//! function state. The caller decides where the argument buffers live: in the per-batch scratch buffer or in the
//! buffer of an indirect command buffer.

mod pull_diag;
mod trace;

use crate::bindless::{Sink, StageInputs};
use crate::detail::{DrawDetail, Stage};
use crate::drawctx::{PassCtx, Prep, Shared, blend_zero_word, cull_of, fixed};
use crate::skip::Skip;
use crate::translate;

/// Translates a draw. `icb` asks for a pipeline that an indirect command buffer can use.
pub(crate) fn prepare(
    sh: &Shared<'_>,
    pc: &PassCtx,
    d: &DrawDetail,
    sink: &mut dyn Sink,
    icb: bool,
) -> Result<Prep, Skip> {
    // An ICB holds direct draws of vertex pipelines only.
    if icb && (d.args.is_mesh() || d.args.is_indirect()) {
        return Err(Skip::Encode);
    }
    if d.args.is_mesh() {
        return crate::mesh::prepare(sh, pc, d, sink);
    }
    let cull = cull_of(d)?;
    let (primitive, _) = translate::primitive(d.topology).ok_or(Skip::Primitive)?;
    let vs = sh.programs.stage(&d.program.vertex, true)?;
    let fs = match &d.program.fragment {
        Some(k) => Some(sh.programs.stage(k, false)?),
        None => None,
    };
    let (pipeline, key) = sh
        .pipelines
        .render(d, &vs, fs.as_deref(), &pc.targets, icb)?;
    let f = fixed(sh, pc, d, cull)?;
    let (geom, base_instance) = crate::geometry::geometry(sh, d, primitive, sink)?;
    let mut streams = Vec::with_capacity(d.streams.len());
    let mut stream_locs = Vec::with_capacity(d.streams.len());
    for s in &d.streams {
        let loc = crate::geometry::stream_loc(sh.pools, s)?;
        if !vs.pull {
            streams.push((translate::stream_slot(s.index), loc.real));
            stream_locs.push(Some(loc));
        }
    }
    if vs.pull {
        let block = crate::pull::layout(d, |a| {
            sh.pools
                .resolve_start_c(a, crate::pools::overlay::STREAM)
                .ok()
                .map(|l| l.real)
        });
        if crate::diag("pull") {
            let inputs: Vec<u32> = vs.inputs.iter().map(|(l, _)| *l).collect();
            pull_diag::diag_draw(sh, d, &inputs);
        }
        streams.push((crate::pull::SLOT, put_layout(sink, &block)?));
        stream_locs.push(None);
    } else if translate::needs_default_vertex(d, &vs.inputs) {
        streams.push((translate::DEFAULT_VERTEX_SLOT, sink.put(&[0; 16])?));
        stream_locs.push(None);
    }
    crate::pull::count(vs.pull);
    let inputs = |stage| StageInputs {
        stage,
        ubos: &d.ubos,
        sbufs: &d.sbufs,
        textures: &d.textures,
        samplers: &d.samplers,
        images: &d.images,
        base_instance,
        groups: None,
        push: d.push.as_deref(),
        blendz: match (&fs, stage) {
            (Some(f), Stage::Fragment) => blend_zero_word(d, f, &pc.targets),
            _ => 0,
        },
    };
    let vertex_args = crate::bindless::fill_args(&vs, &inputs(Stage::Vertex), &sh.env, sink)?;
    let fragment_args = match &fs {
        Some(f) => Some(crate::bindless::fill_args_scaled(
            f,
            &inputs(Stage::Fragment),
            &sh.env,
            pc.scale,
            sink,
        )?),
        None => None,
    };
    if let Some(t) = &crate::settings::live().trace_fs {
        trace::trace_draw(sh, d, t);
    }
    Ok(Prep {
        pipeline,
        key,
        depth_stencil: f.depth_stencil,
        depth_stencil_desc: f.depth_stencil_desc,
        vertex_args,
        fragment_args,
        object_args: None,
        streams,
        stream_locs,
        pulled: vs.pull,
        pooled: crate::bindless::binds_pool(&vs)
            || fs.as_deref().is_some_and(crate::bindless::binds_pool),
        cull: f.cull,
        winding: f.winding,
        fill: f.fill,
        bias: f.bias,
        clip: f.clip,
        geom,
    })
}

/// The address of the layout block of a pulled draw. The draws of a pass that keep their vertex state share one copy
/// through the sink's memo.
fn put_layout(sink: &mut dyn Sink, block: &crate::pull::Block) -> Result<u64, Skip> {
    if let Some(a) = sink.memo().and_then(|m| m.layout(block)) {
        return Ok(a);
    }
    let a = sink.put(block)?;
    if let Some(m) = sink.memo() {
        m.store_layout(block, a);
    }
    Ok(a)
}
