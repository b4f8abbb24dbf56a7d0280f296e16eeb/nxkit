//! The fixed-function state of Vulkan in Metal terms (the reference layer's `blend_f`, `write_mask`, `stencil_d`,
//! `metal_scissor` and the rasterization part of `pipe_make`). Pure functions, no Metal objects.

use nxkit_metal::{
    BlendFactor, BlendOp, BlendState, ColorTarget, CompareFn, CullMode, DepthClip,
    DepthStencilDesc, FillMode, Format, Primitive, Scissor, StencilFace as MStencil, StencilOp,
    TopologyClass, Winding,
};
use nxkit_vk::desc::{BlendAttachment, DepthStencil, Raster, Rect2D, StencilFace};

/// `VkBlendFactor`, in its order (0 zero to 18 one minus source 1 alpha).
pub(crate) fn blend_factor(f: u32) -> BlendFactor {
    use BlendFactor as B;
    const T: [BlendFactor; 19] = [
        B::Zero,
        B::One,
        B::SrcColor,
        B::OneMinusSrcColor,
        B::DstColor,
        B::OneMinusDstColor,
        B::SrcAlpha,
        B::OneMinusSrcAlpha,
        B::DstAlpha,
        B::OneMinusDstAlpha,
        B::BlendColor,
        B::OneMinusBlendColor,
        B::BlendAlpha,
        B::OneMinusBlendAlpha,
        B::SrcAlphaSaturated,
        B::Src1Color,
        B::OneMinusSrc1Color,
        B::Src1Alpha,
        B::OneMinusSrc1Alpha,
    ];
    T.get(f as usize).copied().unwrap_or(B::One)
}

/// `VkBlendOp`: add, subtract, reverse subtract, min, max.
pub(crate) fn blend_op(op: u32) -> BlendOp {
    match op {
        1 => BlendOp::Subtract,
        2 => BlendOp::ReverseSubtract,
        3 => BlendOp::Min,
        4 => BlendOp::Max,
        _ => BlendOp::Add,
    }
}

/// `VkCompareOp`, which has Metal's order: never, less, equal, less or equal, greater, not equal, greater or equal,
/// always.
pub(crate) fn compare(op: u32) -> CompareFn {
    CompareFn::ALL
        .get(op as usize)
        .copied()
        .unwrap_or(CompareFn::Always)
}

/// `VkStencilOp`, which has Metal's order.
pub(crate) fn stencil_op(op: u32) -> StencilOp {
    StencilOp::ALL
        .get(op as usize)
        .copied()
        .unwrap_or(StencilOp::Keep)
}

/// The colour target of attachment `format` with the blend state `b`. Blending is on only for a format that blends
/// (not an integer format); the write mask has Vulkan's bit order, which is `ColorTarget`'s.
pub(crate) fn color_target(format: Format, vk: u32, b: Option<&BlendAttachment>) -> ColorTarget {
    let blends = nxkit_vk::format::lookup(vk).is_some_and(|f| f.is_color() && !f.is_int());
    let mut t = ColorTarget::new(format);
    if let Some(b) = b {
        t.write_mask = (b.write_mask & 0xF) as u8;
        if b.enable && blends {
            t.blend = Some(BlendState {
                src_rgb: blend_factor(b.src_color),
                dst_rgb: blend_factor(b.dst_color),
                op_rgb: blend_op(b.color_op),
                src_alpha: blend_factor(b.src_alpha),
                dst_alpha: blend_factor(b.dst_alpha),
                op_alpha: blend_op(b.alpha_op),
            });
        }
    }
    t
}

fn stencil_face(s: &StencilFace, compare_mask: u32, write_mask: u32) -> MStencil {
    MStencil {
        compare: compare(s.compare),
        fail: stencil_op(s.fail),
        depth_fail: stencil_op(s.depth_fail),
        pass: stencil_op(s.pass),
        read_mask: compare_mask,
        write_mask,
    }
}

/// The stencil masks of a draw: compare and write mask of the front and the back face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Masks {
    /// The front compare mask.
    pub(crate) front_compare: u32,
    /// The front write mask.
    pub(crate) front_write: u32,
    /// The back compare mask.
    pub(crate) back_compare: u32,
    /// The back write mask.
    pub(crate) back_write: u32,
}

impl Masks {
    /// The masks of the pipeline's state.
    pub(crate) fn of(ds: &DepthStencil) -> Self {
        Self {
            front_compare: ds.front.compare_mask,
            front_write: ds.front.write_mask,
            back_compare: ds.back.compare_mask,
            back_write: ds.back.write_mask,
        }
    }
}

/// The depth and stencil state for an attachment of `depth_format` (None without one). Depth is tested only when the
/// format has depth and the test is on, stencil only when it has stencil and the test is on.
pub(crate) fn depth_stencil(
    ds: Option<&DepthStencil>,
    depth_format: Option<u32>,
    masks: Option<Masks>,
) -> DepthStencilDesc {
    let f = depth_format.and_then(nxkit_vk::format::lookup);
    let (Some(ds), Some(f)) = (ds, f) else {
        return DepthStencilDesc::OFF;
    };
    let mut d = DepthStencilDesc::OFF;
    if f.has_depth() && ds.depth_test {
        d.depth_compare = compare(ds.depth_compare);
        d.depth_write = ds.depth_write;
    }
    if f.has_stencil() && ds.stencil_test {
        let m = masks.unwrap_or_else(|| Masks::of(ds));
        d.front = Some(stencil_face(&ds.front, m.front_compare, m.front_write));
        d.back = Some(stencil_face(&ds.back, m.back_compare, m.back_write));
    }
    d
}

/// The rasterization state that a render pass sets with each pipeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RasterState {
    /// The faces that are culled.
    pub(crate) cull: CullMode,
    /// The winding of a front face.
    pub(crate) winding: Winding,
    /// How triangles are filled.
    pub(crate) fill: FillMode,
    /// Clip or clamp the depth.
    pub(crate) clip: DepthClip,
    /// The depth bias (constant, clamp, slope), when it is on.
    pub(crate) bias: Option<[f32; 3]>,
}

/// The rasterization state of a pipeline, and what Metal cannot do (for the log): culling both faces draws nothing in
/// Vulkan and is drawn with culling off, a point fill mode fills.
pub(crate) fn raster(r: &Raster) -> (RasterState, Vec<&'static str>) {
    let mut lacking = Vec::new();
    let cull = match r.cull_mode {
        1 => CullMode::Front,
        2 => CullMode::Back,
        3 => {
            lacking.push("culling both front and back faces");
            CullMode::None
        }
        _ => CullMode::None,
    };
    if r.polygon_mode == 2 {
        lacking.push("the point polygon mode");
    }
    // The shaders flip y, so the winding on the framebuffer is the one the game gave.
    let state = RasterState {
        cull,
        winding: if r.front_face == 1 {
            Winding::Clockwise
        } else {
            Winding::CounterClockwise
        },
        fill: if r.polygon_mode == 1 {
            FillMode::Lines
        } else {
            FillMode::Fill
        },
        clip: if r.depth_clamp {
            DepthClip::Clamp
        } else {
            DepthClip::Clip
        },
        bias: r.depth_bias,
    };
    (state, lacking)
}

/// The primitive and its topology class of a `VkPrimitiveTopology`. None for a topology that Metal does not draw
/// (fans, adjacency, patches).
pub(crate) fn topology(t: u32) -> Option<(Primitive, TopologyClass)> {
    Some(match t {
        0 => (Primitive::Point, TopologyClass::Point),
        1 => (Primitive::Line, TopologyClass::Line),
        2 => (Primitive::LineStrip, TopologyClass::Line),
        3 => (Primitive::Triangle, TopologyClass::Triangle),
        4 => (Primitive::TriangleStrip, TopologyClass::Triangle),
        _ => return None,
    })
}

/// A scissor clamped into a render target of `w` x `h`: Metal requires it to lie inside.
pub(crate) fn scissor(r: Rect2D, w: u32, h: u32) -> Scissor {
    let clamp = |v: i64, hi: u32| v.clamp(0, i64::from(hi));
    let x1 = clamp(i64::from(r.x) + i64::from(r.width), w);
    let y1 = clamp(i64::from(r.y) + i64::from(r.height), h);
    let x0 = clamp(i64::from(r.x), w).min(x1);
    let y0 = clamp(i64::from(r.y), h).min(y1);
    Scissor {
        x: x0 as u32,
        y: y0 as u32,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_and_ops_follow_vulkan_order() {
        assert_eq!(blend_factor(0), BlendFactor::Zero);
        assert_eq!(blend_factor(7), BlendFactor::OneMinusSrcAlpha);
        assert_eq!(blend_factor(10), BlendFactor::BlendColor);
        assert_eq!(blend_factor(14), BlendFactor::SrcAlphaSaturated);
        assert_eq!(blend_factor(18), BlendFactor::OneMinusSrc1Alpha);
        assert_eq!(blend_factor(99), BlendFactor::One);
        assert_eq!(blend_op(2), BlendOp::ReverseSubtract);
        assert_eq!(compare(3), CompareFn::LessEqual);
        assert_eq!(compare(6), CompareFn::GreaterEqual);
        assert_eq!(stencil_op(2), StencilOp::Replace);
        assert_eq!(stencil_op(7), StencilOp::DecrementWrap);
    }

    #[test]
    fn integer_targets_do_not_blend() {
        let b = BlendAttachment {
            enable: true,
            src_color: 6,
            dst_color: 7,
            write_mask: 0b0111,
            ..BlendAttachment::default()
        };
        let t = color_target(Format::Rgba8Unorm, 37, Some(&b));
        assert!(t.blend.is_some());
        assert_eq!(t.write_mask, 0b0111);
        let t = color_target(Format::Rgba8Uint, 41, Some(&b));
        assert!(t.blend.is_none());
        assert_eq!(color_target(Format::Rgba8Unorm, 37, None).write_mask, 0xF);
    }

    #[test]
    fn depth_and_stencil_follow_the_format() {
        let ds = DepthStencil {
            depth_test: true,
            depth_write: true,
            depth_compare: 1,
            stencil_test: true,
            front: StencilFace {
                compare: 7,
                pass: 2,
                compare_mask: 0xFF,
                write_mask: 0x0F,
                ..StencilFace::default()
            },
            ..DepthStencil::default()
        };
        let d = depth_stencil(Some(&ds), Some(126), None);
        assert_eq!((d.depth_compare, d.depth_write), (CompareFn::Less, true));
        assert!(d.front.is_none(), "D32 has no stencil");
        let d = depth_stencil(Some(&ds), Some(130), None);
        let f = d.front.map(|f| (f.pass, f.read_mask, f.write_mask));
        assert_eq!(f, Some((StencilOp::Replace, 0xFF, 0x0F)));
        let m = Masks {
            front_write: 1,
            ..Masks::of(&ds)
        };
        let d = depth_stencil(Some(&ds), Some(130), Some(m));
        assert_eq!(d.front.map(|f| f.write_mask), Some(1));
        assert_eq!(depth_stencil(Some(&ds), None, None), DepthStencilDesc::OFF);
        let off = DepthStencil {
            depth_test: false,
            depth_write: true,
            ..ds
        };
        let d = depth_stencil(Some(&off), Some(126), None);
        assert_eq!((d.depth_compare, d.depth_write), (CompareFn::Always, false));
    }

    #[test]
    fn raster_and_topology() {
        let (s, lacking) = raster(&Raster {
            cull_mode: 2,
            front_face: 1,
            polygon_mode: 1,
            depth_clamp: true,
            ..Raster::default()
        });
        assert_eq!(s.cull, CullMode::Back);
        assert_eq!(s.winding, Winding::Clockwise);
        assert_eq!(s.fill, FillMode::Lines);
        assert_eq!(s.clip, DepthClip::Clamp);
        assert!(lacking.is_empty());
        let (s, lacking) = raster(&Raster {
            cull_mode: 3,
            ..Raster::default()
        });
        assert_eq!((s.cull, lacking.len()), (CullMode::None, 1));
        assert_eq!(
            topology(4),
            Some((Primitive::TriangleStrip, TopologyClass::Triangle))
        );
        assert_eq!(topology(5), None, "fans are not drawn");
    }

    #[test]
    fn scissors_are_clamped_into_the_target() {
        let s = scissor(
            Rect2D {
                x: -10,
                y: 5,
                width: 100,
                height: 2000,
            },
            64,
            48,
        );
        assert_eq!((s.x, s.y, s.width, s.height), (0, 5, 64, 43));
        let s = scissor(
            Rect2D {
                x: 100,
                y: 0,
                width: 10,
                height: 10,
            },
            64,
            48,
        );
        assert_eq!((s.x, s.width), (64, 0));
    }
}
