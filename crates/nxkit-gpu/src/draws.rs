//! The translation of one NVN draw into the Metal objects and values that both encoding paths need.
//!
//! A draw goes into a render pass either command by command or into an indirect command buffer. Both paths call
//! [`prepare`]. It resolves the programs, builds the pipeline and the argument buffers and translates the fixed
//! function state. The caller decides where the argument buffers live: in the per-batch scratch buffer or in the
//! buffer of an indirect command buffer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nxkit_metal::{
    Context, CullMode, DepthBias, DepthClip, DepthStencilDesc, DepthStencilState, FillMode, GuestMemory, IndexType, Primitive,
    RenderPipeline, Winding,
};
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::resource::{ResourceTable, TexId};

use crate::barriers::BarrierPlan;
use crate::bindless::{Env, Sink, StageInputs};
use crate::detail::{DrawArgs, DrawDetail, FrameDetail, RecordingRun, Stage};
use crate::icbs::Icbs;
use crate::pipelines::{Pipelines, Targets};
use crate::programs::Programs;
use crate::stats::Skip;
use crate::textures::Res;
use crate::translate;
use crate::upload::Transfer;

/// How many draws, clears, copies and dispatches come before a command.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ordinal {
    pub(crate) draw: u32,
    pub(crate) clear: u32,
    pub(crate) copy: u32,
    pub(crate) dispatch: u32,
}

/// The ordinals of every command of a frame.
pub(crate) fn ordinals(frame: &Frame) -> Vec<Ordinal> {
    let mut n = Ordinal::default();
    let mut out = Vec::with_capacity(frame.cmds.len());
    for c in &frame.cmds {
        out.push(n);
        match c {
            Cmd::Draw { .. } => n.draw += 1,
            Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => n.clear += 1,
            Cmd::Copy { .. } | Cmd::Downsample { .. } => n.copy += 1,
            Cmd::Dispatch { .. } => n.dispatch += 1,
            _ => {}
        }
    }
    out
}

/// What the encoding tasks of one frame read. Nothing in it changes while the tasks run.
pub(crate) struct Shared<'a> {
    pub(crate) ctx: &'a Context,
    pub(crate) frame: &'a Frame,
    pub(crate) plan: &'a PassPlan,
    pub(crate) detail: &'a FrameDetail,
    pub(crate) table: &'a ResourceTable,
    pub(crate) textures: &'a HashMap<TexId, Res>,
    pub(crate) programs: &'a Programs,
    pub(crate) pipelines: &'a Pipelines,
    pub(crate) env: Env<'a>,
    pub(crate) guest: &'a Arc<GuestMemory>,
    pub(crate) icbs: &'a Mutex<Icbs>,
    pub(crate) ordinals: &'a [Ordinal],
    /// The recording runs by the ordinal of their first draw.
    pub(crate) run_at: &'a HashMap<u32, RecordingRun>,
    pub(crate) barriers: &'a BarrierPlan,
    pub(crate) transfer: &'a Transfer,
    pub(crate) scale: f32,
}

impl Shared<'_> {
    /// The offset of a guest GPU address in guest memory, or `None` when it lies outside.
    pub(crate) fn guest_offset(&self, addr: u64, len: u64) -> Option<u64> {
        let map = self.guest.address_map();
        let off = addr.checked_sub(map.gpu_base())?;
        (off.checked_add(len)? <= map.len()).then_some(off)
    }
}

/// What a render pass looks like to its draws.
#[derive(Clone, Debug)]
pub(crate) struct PassCtx {
    /// The colour formats and the sample count.
    pub(crate) targets: Targets,
    /// The size of the attachments in texels, after scaling.
    pub(crate) size: [u32; 2],
    /// The factor that the NVN viewport and scissor get. 1 when the attachments are not scaled.
    pub(crate) scale: f32,
    /// True when the pass has a depth attachment.
    pub(crate) has_depth: bool,
    /// True when the depth attachment has a stencil aspect.
    pub(crate) has_stencil: bool,
}

/// The geometry of a draw in Metal terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Geom {
    Arrays { primitive: Primitive, first: u32, count: u32, instances: u32, base_instance: u32 },
    Indexed {
        primitive: Primitive,
        index_type: IndexType,
        address: u64,
        count: u32,
        instances: u32,
        base_vertex: i32,
        base_instance: u32,
    },
}

/// A draw that is ready to encode.
pub(crate) struct Prep {
    pub(crate) pipeline: RenderPipeline,
    pub(crate) key: u64,
    pub(crate) depth_stencil: DepthStencilState,
    pub(crate) depth_stencil_desc: DepthStencilDesc,
    pub(crate) vertex_args: u64,
    pub(crate) fragment_args: Option<u64>,
    /// The buffer slot and guest GPU address of each vertex stream.
    pub(crate) streams: Vec<(u32, u64)>,
    pub(crate) cull: CullMode,
    pub(crate) winding: Winding,
    pub(crate) fill: FillMode,
    pub(crate) bias: DepthBias,
    pub(crate) clip: DepthClip,
    pub(crate) geom: Geom,
}

/// Translates a draw. `icb` asks for a pipeline that an indirect command buffer can use.
pub(crate) fn prepare(
    sh: &Shared<'_>,
    pc: &PassCtx,
    d: &DrawDetail,
    sink: &mut dyn Sink,
    icb: bool,
) -> Result<Prep, Skip> {
    let cull = translate::cull(d.raster.cull).ok_or(Skip::Cull)?;
    let (primitive, _) = translate::primitive(d.topology).ok_or(Skip::Primitive)?;
    let vs = sh.programs.stage(&d.program.vertex, true)?;
    let fs = match &d.program.fragment {
        Some(k) => Some(sh.programs.stage(k, false)?),
        None => None,
    };
    let (pipeline, key) = sh.pipelines.render(d, &vs, fs.as_deref(), &pc.targets, icb)?;
    let mut ds = d.depth_stencil;
    ds.depth_test &= pc.has_depth;
    ds.depth_write &= pc.has_depth;
    ds.stencil_test &= pc.has_stencil;
    let depth_stencil_desc = translate::depth_stencil(&ds);
    let depth_stencil = sh.pipelines.depth_stencil(&depth_stencil_desc)?;
    let (geom, base_instance) = geometry(sh, d, primitive)?;
    let mut streams = Vec::with_capacity(d.streams.len());
    for s in &d.streams {
        sh.guest_offset(s.address, s.size.max(1)).ok_or(Skip::Buffer)?;
        streams.push((translate::stream_slot(s.index), s.address));
    }
    let inputs = |stage| StageInputs { stage, ubos: &d.ubos, textures: &d.textures, base_instance, groups: None };
    let vertex_args = crate::bindless::fill_args(&vs, &inputs(Stage::Vertex), &sh.env, sink)?;
    let fragment_args = match &fs {
        Some(f) => Some(crate::bindless::fill_args(f, &inputs(Stage::Fragment), &sh.env, sink)?),
        None => None,
    };
    let [factor, units, clamp] = d.raster.depth_bias.unwrap_or([0.0; 3]);
    Ok(Prep {
        pipeline,
        key,
        depth_stencil,
        depth_stencil_desc,
        vertex_args,
        fragment_args,
        streams,
        cull,
        winding: translate::winding(d.raster.front_face),
        fill: translate::fill(d.raster.polygon_mode),
        bias: DepthBias { constant: units, slope_scale: factor, clamp },
        clip: if d.raster.depth_clamp { DepthClip::Clamp } else { DepthClip::Clip },
        geom,
    })
}

fn geometry(sh: &Shared<'_>, d: &DrawDetail, primitive: Primitive) -> Result<(Geom, u32), Skip> {
    Ok(match d.args {
        DrawArgs::Arrays { first, count, instances, base_instance } => {
            (Geom::Arrays { primitive, first, count, instances, base_instance }, base_instance)
        }
        DrawArgs::Indexed { index_type, address, count, instances, base_vertex, base_instance } => {
            let it = translate::index_type(index_type).ok_or(Skip::Index)?;
            sh.guest_offset(address, u64::from(count) * it.bytes()).ok_or(Skip::Buffer)?;
            (
                Geom::Indexed {
                    primitive,
                    index_type: it,
                    address,
                    count,
                    instances,
                    base_vertex,
                    base_instance,
                },
                base_instance,
            )
        }
    })
}
