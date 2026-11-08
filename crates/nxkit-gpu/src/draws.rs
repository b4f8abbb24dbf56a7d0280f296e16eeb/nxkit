//! The translation of one NVN draw into the Metal objects and values that both encoding paths need.
//!
//! A draw goes into a render pass either command by command or into an indirect command buffer. Both paths call
//! [`prepare`]. It resolves the programs, builds the pipeline and the argument buffers and translates the fixed
//! function state. The caller decides where the argument buffers live: in the per-batch scratch buffer or in the
//! buffer of an indirect command buffer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nxkit_metal::{
    Context, CullMode, DepthBias, DepthClip, DepthStencilDesc, DepthStencilState, FillMode,
    GuestMemory, IndexType, Primitive, RenderPipeline, Winding,
};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::resource::TexId;

use crate::barriers::BarrierPlan;
use crate::bindless::{Env, Sink, StageInputs};
use crate::detail::{DrawArgs, DrawDetail, FrameDetail, RecordingRun, Stage};
use crate::icbs::Icbs;
use crate::pipelines::{Pipelines, Targets};
use crate::programs::Programs;
use crate::snapshot::EncodeView;
use crate::stats::Skip;
use crate::textures::Res;
use crate::translate;
use crate::upload::Transfer;

/// A map with the fast hasher of `foldhash`. The keys of the per-draw maps are small integers and short keys that do
/// not need the protection of the default hasher. Build one with `FastMap::default()`.
pub(crate) type FastMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

/// A set with the fast hasher of `foldhash`. Build one with `FastSet::default()`.
pub(crate) type FastSet<K> = std::collections::HashSet<K, foldhash::fast::RandomState>;

/// Whether `NXKIT_DIAG` asks for the depth diagnostic. It is read once, because [`prepare`] runs for every draw.
fn diag_depth() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::diag("depth"))
}

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
    pub(crate) table: &'a dyn EncodeView,
    pub(crate) textures: &'a FastMap<TexId, Res>,
    pub(crate) texture_store: &'a crate::textures::TextureStore,
    pub(crate) programs: &'a Programs,
    pub(crate) pipelines: &'a Pipelines,
    pub(crate) env: Env<'a>,
    pub(crate) guest: &'a Arc<GuestMemory>,
    pub(crate) icbs: &'a Mutex<Icbs>,
    pub(crate) ordinals: &'a [Ordinal],
    /// The recording runs by the ordinal of their first draw.
    pub(crate) run_at: &'a FastMap<u32, RecordingRun>,
    pub(crate) barriers: &'a BarrierPlan,
    pub(crate) transfer: &'a Transfer,
    /// The index of the first pass of the frame that writes a texture.
    pub(crate) first_write: &'a FastMap<TexId, usize>,
    pub(crate) scale: f32,
    /// The scratch address of each uniform snapshot that the batch already uploaded, by the address of its bytes.
    pub(crate) uploaded: &'a Mutex<FastMap<usize, u64>>,
    /// The snapshots that were uploaded before the chunks started. A lookup needs no lock.
    pub(crate) preloaded: &'a FastMap<usize, u64>,
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
    Arrays {
        primitive: Primitive,
        first: u32,
        count: u32,
        instances: u32,
        base_instance: u32,
    },
    Indexed {
        primitive: Primitive,
        index_type: IndexType,
        address: u64,
        count: u32,
        instances: u32,
        base_vertex: i32,
        base_instance: u32,
    },
    /// An indirect draw: the GPU reads the arguments from guest memory at `indirect`.
    Indirect { primitive: Primitive, indirect: u64 },
    /// An indexed indirect draw. `index_bytes` is the guest memory from `address` to its end.
    IndexedIndirect {
        primitive: Primitive,
        index_type: IndexType,
        address: u64,
        index_bytes: u64,
        indirect: u64,
    },
    /// The `draws` indirect draws of a multi-draw. The compute pre-pass of the render pass (`indirect`) copies their
    /// arguments, with zeros past the count, [`crate::indirect::RECORD`] bytes apart.
    MultiIndirect {
        primitive: Primitive,
        /// The index type, the address of index 0 and the bytes to the end of guest memory of an elements draw.
        index: Option<(IndexType, u64, u64)>,
        draws: u32,
    },
    /// A task and mesh draw of `count` workgroups. `first` is the GPU address of the first workgroup of each grid of
    /// at most [`crate::mesh::MAX_GRID`] workgroups, 16 bytes apart. A draw with a task shader is one grid.
    Mesh {
        first: u64,
        count: u32,
        object_threads: u32,
        mesh_threads: u32,
        task: bool,
    },
    /// An indirect task and mesh draw. The pre-pass of the render pass turns `{count, first}` into `splits` grids.
    MeshIndirect {
        splits: u32,
        object_threads: u32,
        mesh_threads: u32,
        task: bool,
    },
}

/// A draw that is ready to encode.
pub(crate) struct Prep {
    pub(crate) pipeline: RenderPipeline,
    pub(crate) key: u64,
    pub(crate) depth_stencil: DepthStencilState,
    pub(crate) depth_stencil_desc: DepthStencilDesc,
    /// The argument buffer of the vertex stage, or of the mesh stage of a mesh draw.
    pub(crate) vertex_args: u64,
    pub(crate) fragment_args: Option<u64>,
    /// The argument buffer of the task shader of a mesh draw.
    pub(crate) object_args: Option<u64>,
    /// The buffer slot and guest GPU address of each vertex stream.
    pub(crate) streams: Vec<(u32, u64)>,
    pub(crate) cull: CullMode,
    pub(crate) winding: Winding,
    pub(crate) fill: FillMode,
    pub(crate) bias: DepthBias,
    pub(crate) clip: DepthClip,
    pub(crate) geom: Geom,
}

/// The fixed function state of a draw in Metal terms.
pub(crate) struct Fixed {
    pub(crate) cull: CullMode,
    pub(crate) depth_stencil: DepthStencilState,
    pub(crate) depth_stencil_desc: DepthStencilDesc,
    pub(crate) winding: Winding,
    pub(crate) fill: FillMode,
    pub(crate) bias: DepthBias,
    pub(crate) clip: DepthClip,
}

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
    let (geom, base_instance) = geometry(sh, d, primitive, sink)?;
    let mut streams = Vec::with_capacity(d.streams.len());
    for s in &d.streams {
        if sh.guest_offset(s.address, s.size.max(1)).is_none() {
            let m = sh.guest.address_map();
            return Err(crate::buf_skip(format_args!(
                "stream {} addr {:#x} size {:#x} base {:#x} len {:#x}",
                s.index,
                s.address,
                s.size,
                m.gpu_base(),
                m.len()
            )));
        }
        streams.push((translate::stream_slot(s.index), s.address));
    }
    if translate::needs_default_vertex(d, &vs.inputs) {
        streams.push((translate::DEFAULT_VERTEX_SLOT, sink.put(&[0; 16])?));
    }
    let inputs = |stage| StageInputs {
        stage,
        ubos: &d.ubos,
        sbufs: &d.sbufs,
        textures: &d.textures,
        images: &d.images,
        base_instance,
        groups: None,
        push: d.push.as_deref(),
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
    Ok(Prep {
        pipeline,
        key,
        depth_stencil: f.depth_stencil,
        depth_stencil_desc: f.depth_stencil_desc,
        vertex_args,
        fragment_args,
        object_args: None,
        streams,
        cull: f.cull,
        winding: f.winding,
        fill: f.fill,
        bias: f.bias,
        clip: f.clip,
        geom,
    })
}

/// The cull mode of a draw. NVN culls both faces with `FRONT_AND_BACK`, which Metal cannot.
pub(crate) fn cull_of(d: &DrawDetail) -> Result<CullMode, Skip> {
    translate::cull(if crate::knob("nocull") {
        0
    } else {
        d.raster.cull
    })
    .ok_or(Skip::Cull)
}

/// The depth, stencil and raster state of a draw in a pass, with the depth and stencil state object.
pub(crate) fn fixed(
    sh: &Shared<'_>,
    pc: &PassCtx,
    d: &DrawDetail,
    cull: CullMode,
) -> Result<Fixed, Skip> {
    let mut ds = d.depth_stencil;
    ds.depth_test &= pc.has_depth;
    ds.depth_write &= pc.has_depth;
    ds.stencil_test &= pc.has_stencil;
    if diag_depth() {
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let key = format!(
            "{ds:?} range {:?} depth {} stencil {}",
            d.dynamic.depth_range, pc.has_depth, pc.has_stencil
        );
        if let Ok(mut s) = SEEN.lock() {
            if s.len() < 40 && !s.contains(&key) {
                eprintln!("DIAG depth draw {key}");
                s.push(key);
            }
        }
    }
    if crate::knob("nodepth") {
        ds.depth_test = false;
        ds.stencil_test = false;
    }
    let depth_stencil_desc = translate::depth_stencil(&ds);
    let depth_stencil = sh.pipelines.depth_stencil(&depth_stencil_desc)?;
    let [factor, units, clamp] = if crate::knob("nooffset") {
        [0.0; 3]
    } else {
        d.raster.depth_bias.unwrap_or([0.0; 3])
    };
    Ok(Fixed {
        cull,
        depth_stencil,
        depth_stencil_desc,
        winding: translate::winding(d.raster.front_face),
        fill: translate::fill(d.raster.polygon_mode),
        bias: DepthBias {
            constant: units,
            slope_scale: factor,
            clamp,
        },
        clip: if d.raster.depth_clamp {
            DepthClip::Clamp
        } else {
            DepthClip::Clip
        },
    })
}

fn geometry(
    sh: &Shared<'_>,
    d: &DrawDetail,
    primitive: Primitive,
    sink: &mut dyn Sink,
) -> Result<(Geom, u32), Skip> {
    Ok(match d.args {
        DrawArgs::Arrays {
            first,
            count,
            instances,
            base_instance,
        } => (
            Geom::Arrays {
                primitive,
                first,
                count,
                instances,
                base_instance,
            },
            base_instance,
        ),
        DrawArgs::Indexed {
            index_type,
            address,
            count,
            instances,
            base_vertex,
            base_instance,
        } => {
            // Metal has no 8-bit indices: the indices are widened to 16 bits into the batch's scratch memory, and the
            // draw reads them there (NVN_INDEX_TYPE_UNSIGNED_BYTE, Ocarina of Time's interface and movie quads).
            let (it, address) = if index_type == 0 {
                (IndexType::U16, widen_u8_indices(sh, address, count, sink)?)
            } else {
                (
                    translate::index_type(index_type).ok_or(Skip::Index)?,
                    address,
                )
            };
            if index_type != 0
                && sh
                    .guest_offset(address, u64::from(count) * it.bytes())
                    .is_none()
            {
                return Err(crate::buf_skip(format_args!(
                    "index addr {address:#x} count {count}"
                )));
            }
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
        // The GPU reads the first instance with the arguments, so the driver block of an indirect draw has 0.
        DrawArgs::ArraysIndirect { indirect } => {
            indirect_args(sh, indirect, 16)?;
            (
                Geom::Indirect {
                    primitive,
                    indirect,
                },
                0,
            )
        }
        DrawArgs::IndexedIndirect {
            index_type,
            address,
            indirect,
        } => {
            let it = translate::index_type(index_type).ok_or(Skip::Index)?;
            indirect_args(sh, indirect, 20)?;
            let index_bytes = index_room(sh, address)?;
            (
                Geom::IndexedIndirect {
                    primitive,
                    index_type: it,
                    address,
                    index_bytes,
                    indirect,
                },
                0,
            )
        }
        DrawArgs::MultiIndirectCount { index, max, .. } => {
            let index = match index {
                Some((ty, address)) => {
                    let it = translate::index_type(ty).ok_or(Skip::Index)?;
                    Some((it, address, index_room(sh, address)?))
                }
                None => None,
            };
            (
                Geom::MultiIndirect {
                    primitive,
                    index,
                    draws: max.min(crate::indirect::MAX_MULTI),
                },
                0,
            )
        }
        DrawArgs::MeshTasks { .. } | DrawArgs::MeshTasksIndirect { .. } => {
            return Err(Skip::Detail);
        }
    })
}

/// Reads `count` 8-bit indices at the guest GPU address `addr`, widens them to 16 bits and puts them into the sink.
/// Returns the address of the 16-bit indices.
fn widen_u8_indices(
    sh: &Shared<'_>,
    addr: u64,
    count: u32,
    sink: &mut dyn Sink,
) -> Result<u64, Skip> {
    let off = sh
        .guest_offset(addr, u64::from(count))
        .ok_or_else(|| crate::buf_skip(format_args!("8-bit index addr {addr:#x} count {count}")))?;
    let mut bytes = vec![0u8; count as usize];
    sh.guest.read(off, &mut bytes).map_err(|_| {
        crate::buf_skip(format_args!(
            "8-bit index addr {addr:#x} count {count}: unreadable"
        ))
    })?;
    let wide: Vec<u8> = bytes
        .iter()
        .flat_map(|&b| u16::from(b).to_le_bytes())
        .collect();
    sink.put(&wide)
}

/// Checks that `bytes` of indirect arguments at `addr` lie in guest memory at a 4 byte aligned address.
pub(crate) fn indirect_args(sh: &Shared<'_>, addr: u64, bytes: u64) -> Result<(), Skip> {
    if !addr.is_multiple_of(4) || sh.guest_offset(addr, bytes).is_none() {
        return Err(crate::buf_skip(format_args!(
            "indirect arguments addr {addr:#x} bytes {bytes}"
        )));
    }
    Ok(())
}

/// The bytes of guest memory from the index buffer at `addr` to its end. An indirect draw takes its index count from
/// the GPU, so the encoder gives Metal all of it; Metal reads an index past it as 0.
fn index_room(sh: &Shared<'_>, addr: u64) -> Result<u64, Skip> {
    let off = sh
        .guest_offset(addr, 1)
        .ok_or_else(|| crate::buf_skip(format_args!("index addr {addr:#x}")))?;
    Ok((sh.guest.address_map().len() - off).min(u64::from(u32::MAX)))
}
