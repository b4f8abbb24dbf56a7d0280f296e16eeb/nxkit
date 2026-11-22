//! Partial clears: an `nvnCommandBufferClearColor` or `ClearDepthStencil` under a scissor that does not hold the
//! target, or a colour clear of some channels. A full clear is the load action of its pass. A partial one stays inside
//! the pass (`nxkit_nvn::analyse`) and becomes a draw here.
//!
//! The draw is one full-screen triangle under the scissor of the clear, with no buffer and no texture:
//!
//! - colour: the fragment function writes 1 to the cleared slot, and the blend `src * constant + dst * 0` makes it the
//!   clear colour, which the blend colour of the pass carries. The write mask is the channel mask of the clear. The
//!   other slots keep their contents (write mask 0). An integer target cannot blend and is skipped (`Skip::Format`).
//! - depth: the viewport maps every depth to the clear depth (`z_near = z_far`), the test always passes and the depth
//!   is written when the clear writes it.
//! - stencil: the test always passes and `Replace` writes the reference, which is the clear value, into the bits of
//!   the stencil mask of the clear.
//!
//! Pokémon Pokopia renders at a dynamic resolution into a corner of its targets and clears that corner with a scissor:
//! without these draws the stencil of a frame kept the marks of the frames before, its background left them out, and
//! every moving thing left a trail.

use nxkit_metal::{
    BlendFactor, BlendOp, BlendState, ColorTarget, CompareFn, StencilFace, StencilOp,
};
use nxkit_nvn::frame::Frame;

use super::*;

const LIBRARY: &str = "nxkit-clear";

const MSL: &str = r"
#include <metal_stdlib>
using namespace metal;

struct ClearV { float4 pos [[position]]; };

vertex ClearV nxkit_clear_vs(uint id [[vertex_id]]) {
    float2 p = float2((id << 1) & 2, id & 2);
    ClearV v;
    v.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    return v;
}
";

/// The fragment functions, one per colour slot. Metal names the output slot in the function, so each slot has its own.
const FRAGMENTS: [&str; 8] = [
    "nxkit_clear_fs0",
    "nxkit_clear_fs1",
    "nxkit_clear_fs2",
    "nxkit_clear_fs3",
    "nxkit_clear_fs4",
    "nxkit_clear_fs5",
    "nxkit_clear_fs6",
    "nxkit_clear_fs7",
];

/// The library source: the vertex function and a fragment function per colour slot that writes 1 to that slot.
fn source() -> &'static str {
    static SOURCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SOURCE.get_or_init(|| {
    let mut s = String::from(MSL);
    for n in 0..FRAGMENTS.len() {
        s.push_str(&format!(
            "struct ClearF{n} {{ float4 c [[color({n})]]; }};\n\
             fragment ClearF{n} nxkit_clear_fs{n}(ClearV in [[stage_in]]) {{ return {{ float4(1.0) }}; }}\n"
        ));
    }
    s
    })
}

/// The commands of `pass` that are partial clears, with the draw of the pass that each one comes before: `(draw
/// ordinal, command index)`. A clear after the last draw has the ordinal `range.end`.
pub(super) fn partial_clears(frame: &Frame, pass: &Pass, first_draw: u32) -> Vec<(u32, u32)> {
    let mut draws = first_draw;
    let mut out = Vec::new();
    for i in pass.cmds.clone() {
        match frame.cmds.get(i as usize) {
            Some(Cmd::Draw { .. }) => draws += 1,
            Some(Cmd::ClearColor { full: false, .. }) => out.push((draws, i)),
            Some(Cmd::ClearDepthStencil { full: false, keeps }) if *keeps != ClearKeeps::Both => {
                out.push((draws, i))
            }
            _ => {}
        }
    }
    out
}

/// What a partial clear writes, from its command and its detail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Clear {
    /// Colour slot `slot` with `color` in the channels of `mask`.
    Color { slot: u8, color: [f32; 4], mask: u8 },
    /// Depth (when `depth` is some) and stencil (when `stencil` is some, the value and the bits it writes).
    DepthStencil {
        depth: Option<f32>,
        stencil: Option<(u32, u32)>,
    },
}

impl Clear {
    /// The clear of command `cmd` with its detail `d`, or none when it writes nothing.
    pub(super) fn of(cmd: &Cmd, d: &ClearDetail) -> Option<Self> {
        match *cmd {
            Cmd::ClearColor { slot, .. } => (d.color_mask & 0xf != 0).then_some(Self::Color {
                slot,
                color: d.color,
                mask: d.color_mask & 0xf,
            }),
            Cmd::ClearDepthStencil { keeps, .. } => {
                let depth = (!keeps.depth() && d.depth_write).then_some(d.depth);
                let stencil = (!keeps.stencil() && d.stencil_mask & 0xff != 0)
                    .then_some((d.stencil & 0xff, d.stencil_mask & 0xff));
                (depth.is_some() || stencil.is_some())
                    .then_some(Self::DepthStencil { depth, stencil })
            }
            _ => None,
        }
    }

    /// The colour targets of the pipeline: every slot of the pass with its format, written only by a colour clear of
    /// that slot.
    fn colors(&self, targets: &Targets) -> Result<Vec<Option<ColorTarget>>, Skip> {
        let mut out = Vec::with_capacity(targets.colors.len());
        for (n, f) in targets.colors.iter().enumerate() {
            out.push(f.map(|format| {
                let mut t = ColorTarget::new(format);
                t.write_mask = 0;
                t
            }));
            if let (Self::Color { slot, mask, .. }, Some(format)) = (self, f)
                && usize::from(*slot) == n
            {
                if crate::format_map::is_integer(*format) {
                    return Err(Skip::Format);
                }
                out[n] = Some(ColorTarget {
                    format: *format,
                    blend: Some(BlendState {
                        src_rgb: BlendFactor::BlendColor,
                        dst_rgb: BlendFactor::Zero,
                        op_rgb: BlendOp::Add,
                        src_alpha: BlendFactor::BlendAlpha,
                        dst_alpha: BlendFactor::Zero,
                        op_alpha: BlendOp::Add,
                    }),
                    write_mask: *mask,
                });
            }
        }
        Ok(out)
    }

    /// The depth and stencil state of the draw.
    fn depth_stencil(&self) -> DepthStencilDesc {
        let Self::DepthStencil { depth, stencil } = *self else {
            return DepthStencilDesc::OFF;
        };
        let face = stencil.map(|(_, mask)| StencilFace {
            compare: CompareFn::Always,
            fail: StencilOp::Keep,
            depth_fail: StencilOp::Keep,
            pass: StencilOp::Replace,
            read_mask: 0xff,
            write_mask: mask,
        });
        DepthStencilDesc {
            depth_compare: CompareFn::Always,
            depth_write: depth.is_some(),
            front: face,
            back: face,
        }
    }
}

/// Encodes the partial clear of command `cmd` into `rp`. The tracker forgets what it set, since the draw changes the
/// pipeline and the dynamic state.
pub(super) fn encode(
    sh: &EncodeCtx<'_>,
    pc: &PassCtx,
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    cmd: u32,
    stats: &mut EncodeStats,
) {
    let Some(c) = sh.frame.cmds.get(cmd as usize) else {
        return;
    };
    let at = sh
        .ordinals
        .get(cmd as usize)
        .map_or(0, |o| o.clear as usize);
    let detail = sh.detail.clears.get(at).copied().unwrap_or_default();
    let Some(clear) = Clear::of(c, &detail) else {
        return;
    };
    let fits = match clear {
        Clear::Color { slot, .. } => pc
            .targets
            .colors
            .get(usize::from(slot))
            .is_some_and(Option::is_some),
        Clear::DepthStencil { depth, stencil } => {
            (depth.is_none() || pc.has_depth) && (stencil.is_none() || pc.has_stencil)
        }
    };
    if !fits {
        return stats.skip(Skip::Target);
    }
    if let Err(why) = encode_clear(sh, pc, rp, tr, &clear, detail.scissor) {
        stats.skip(why);
        return;
    }
    stats.partial_clears += 1;
}

fn encode_clear(
    sh: &EncodeCtx<'_>,
    pc: &PassCtx,
    rp: &mut RenderPass<'_>,
    tr: &mut Tracker,
    clear: &Clear,
    scissor: Option<[u32; 4]>,
) -> Result<(), Skip> {
    let colors = clear.colors(&pc.targets)?;
    let fragment = match clear {
        Clear::Color { slot, .. } => FRAGMENTS.get(usize::from(*slot)).copied(),
        Clear::DepthStencil { .. } => None,
    };
    let mut h = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&(fragment, &colors, pc.targets.samples), &mut h);
    let pipeline = sh.pipelines.host_render(
        LIBRARY,
        source(),
        ("nxkit_clear_vs", fragment),
        colors,
        pc.targets.samples,
        std::hash::Hasher::finish(&h),
    )?;
    let state = sh.pipelines.depth_stencil(&clear.depth_stencil())?;
    let [w, h] = pc.size;
    let depth = match clear {
        Clear::DepthStencil { depth: Some(d), .. } => f64::from(*d),
        _ => 0.0,
    };
    let (color, reference) = match *clear {
        Clear::Color { color, .. } => (color, 0),
        Clear::DepthStencil { stencil, .. } => ([0.0; 4], stencil.map_or(0, |(v, _)| v)),
    };
    let cmds = [
        RenderCommand::SetPipeline(pipeline),
        RenderCommand::SetDepthStencil(state),
        RenderCommand::SetViewport(Viewport {
            x: 0.0,
            y: 0.0,
            width: f64::from(w),
            height: f64::from(h),
            z_near: depth,
            z_far: depth,
        }),
        RenderCommand::SetScissor(tr.scissor(scissor, pc.scale)),
        RenderCommand::SetCullMode(CullMode::None),
        RenderCommand::SetFillMode(FillMode::Fill),
        RenderCommand::SetDepthBias(DepthBias {
            constant: 0.0,
            slope_scale: 0.0,
            clamp: 0.0,
        }),
        RenderCommand::SetDepthClip(DepthClip::Clamp),
        RenderCommand::SetStencilRef {
            front: reference,
            back: reference,
        },
        RenderCommand::SetBlendColor(color),
        RenderCommand::Draw {
            primitive: nxkit_metal::Primitive::Triangle,
            first_vertex: 0,
            vertex_count: 3,
            instance_count: 1,
            base_instance: 0,
        },
    ];
    tr.forget();
    for c in &cmds {
        rp.encode(c).map_err(|_| Skip::Encode)?;
    }
    Ok(())
}
