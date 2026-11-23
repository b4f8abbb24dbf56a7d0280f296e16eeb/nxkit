//! What the planning of a draw reads and gives: the frame context [`Shared`], the pass of a draw ([`PassCtx`]), a
//! draw in Metal terms ([`Geom`], [`Fixed`], [`Prep`]) and the fixed function state. `draws`, `mesh`, `geometry`,
//! `indirect` and the native replacements build on it, so it sits below them (W29).

use std::sync::Mutex;

use nxkit_metal::{
    Context, CullMode, DepthBias, DepthClip, DepthStencilDesc, DepthStencilState, FillMode,
    GuestSpace, IndexType, Primitive, RenderPipeline, Winding,
};
use nxkit_nvn::frame::Frame;
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::resource::TexId;

use crate::barriers::BarrierPlan;
use crate::bindless::Env;
use crate::detail::{DrawDetail, FrameDetail, RecordingRun};
use crate::fastmap::FastMap;
use crate::ordinal::Ordinal;
use crate::pipelines::{Pipelines, Targets};
use crate::programs::Programs;
use crate::skip::Skip;
use crate::snapshot::EncodeView;
use crate::textures::Res;
use crate::translate;
use crate::upload::Transfer;

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
    pub(crate) guest: &'a GuestSpace,
    /// The pools at the flush of the frame. The batch keeps them alive.
    pub(crate) pools: &'a crate::guestpools::GuestPools,
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
    /// Reads guest memory as the frame sees it: a member page comes from the bytes that the flush staged or from the
    /// mirror, the rest from guest memory. A page that the GPU wrote is stale in guest memory, so a CPU read that feeds the
    /// frame (the indices that are widened, the data that is scanned) goes through here. False when a part is unreadable.
    pub(crate) fn read_guest(&self, addr: u64, out: &mut [u8]) -> bool {
        let guest = |a: u64, part: &mut [u8]| {
            self.guest_offset(a, part.len() as u64)
                .is_some_and(|off| self.guest.read(off, part).is_ok())
        };
        match self.pools.overlay() {
            Some(o) => o.read_with(addr, out, &mut |a, part| guest(a, part)),
            None => guest(addr, out),
        }
    }

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
        /// The real address of the index 0, or an address in the sink for widened 8-bit indices.
        address: u64,
        /// Where the indices lie in the pools, `None` for indices in the sink.
        loc: Option<crate::pools::Loc>,
        count: u32,
        instances: u32,
        base_vertex: i32,
        base_instance: u32,
    },
    /// An indirect draw: the GPU reads the arguments at the real address `indirect`.
    Indirect { primitive: Primitive, indirect: u64 },
    /// An indexed indirect draw. `address` and `indirect` are real addresses. `index_bytes` is the room from `address`
    /// to the end of its pool.
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
        /// The index type, the real address of index 0 and the bytes to the end of its pool of an elements draw.
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
    /// The buffer slot and address of each vertex stream. The address of a stream in guest memory is its real address.
    /// For a pulled draw only the layout block at `pull::SLOT`, an address in the sink like the default vertex at
    /// `translate::DEFAULT_VERTEX_SLOT`.
    pub(crate) streams: Vec<(u32, u64)>,
    /// Where each entry of `streams` lies, in the same order: `Some` for a stream in guest memory (its `real` is the
    /// address in `streams`), `None` for an entry in the sink.
    pub(crate) stream_locs: Vec<Option<crate::pools::Loc>>,
    /// The vertex function pulls its inputs (`crate::pull`).
    pub(crate) pulled: bool,
    /// A stage of the draw has a `tex_pool` or `samp_pool` binding: its argument buffer holds the address of the pool
    /// table of the batch slot of the frame.
    pub(crate) pooled: bool,
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

/// Whether `NXKIT_DIAG` asks for the depth diagnostic. A memo keeps the answer, because [`prepare`] runs for every draw.
fn diag_depth() -> bool {
    static ON: nxkit_abi::live::Memo = nxkit_abi::live::Memo::new();
    ON.flag(|| crate::diag("depth"))
}

/// The cull mode of a draw. NVN culls both faces with `FRONT_AND_BACK`, which Metal cannot.
pub(crate) fn cull_of(d: &DrawDetail) -> Result<CullMode, Skip> {
    // With rasterizer discard nothing reaches the culling, and the stores of the vertex stage still run.
    if d.raster.discard {
        return Ok(CullMode::None);
    }
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
        if let Ok(mut s) = SEEN.lock()
            && s.len() < 40
            && !s.contains(&key)
        {
            eprintln!("DIAG depth draw {key}");
            s.push(key);
        }
    }
    if crate::knob("nodepth") {
        ds.depth_test = false;
        ds.stencil_test = false;
    }
    if crate::knob("nostencil") {
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

/// The blend word of a draw for the pixel shader `fs`: 3 bits per colour target, the kind of its source RGB factor
/// (see `translate::blend_zero_kind`). The shader writes RGB 0 for a pixel whose factor is exactly 0, because the console's
/// blender gives 0 for `0 x NaN` and Metal's gives NaN. The word is 0 for the knob `nobz`.
pub(crate) fn blend_zero_word(
    d: &DrawDetail,
    fs: &crate::programs::StageProgram,
    targets: &crate::pipelines::Targets,
) -> u32 {
    translate::blend_zero_bits(
        &d.blend,
        &targets.colors,
        fs.colors.as_deref(),
        crate::knob("nobz"),
    )
}
