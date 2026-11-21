//! The stage input and output structs, the entry parameters of the built-in inputs and the lookups of the stage IO.
//!
//! Vertex inputs and fragment varyings are members of `main0_in`. The built-ins that Metal only takes as entry
//! parameters (vertex, instance, thread and lane numbers) are passed to every helper by value.

use super::Varying;
use super::emit::{R, Shader};
use super::ioidx;
use super::types::msl_type;
use crate::ir::agg::AggType;
use crate::ir::{IoVariable, StorageKind};
use crate::props::Props;
use crate::structured::StructInfo;
use std::collections::BTreeSet;

/// A built-in that is an entry parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Builtin {
    /// The parameter name.
    pub name: &'static str,
    /// The MSL type.
    pub ty: &'static str,
    /// The attribute.
    pub attr: &'static str,
}

const VERTEX_ID: Builtin = Builtin {
    name: "gl_VertexIndex",
    ty: "uint",
    attr: "vertex_id",
};
const INSTANCE_ID: Builtin = Builtin {
    name: "gl_InstanceIndex",
    ty: "uint",
    attr: "instance_id",
};
const BASE_VERTEX: Builtin = Builtin {
    name: "gl_BaseVertex",
    ty: "uint",
    attr: "base_vertex",
};
/// The base instance parameter.
pub const BASE_INSTANCE: Builtin = Builtin {
    name: "gl_BaseInstance",
    ty: "uint",
    attr: "base_instance",
};
const LANE: Builtin = Builtin {
    name: "gl_SubgroupInvocationID",
    ty: "uint",
    attr: "thread_index_in_simdgroup",
};

/// One member of the fragment output struct, kept as data for the entries that return a changed struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FragOut {
    /// The member name (`c0`, `c1_pad`, `gl_FragDepth`).
    pub name: String,
    /// The MSL type.
    pub ty: String,
    /// The attribute text without the brackets (`color(0)`, `color(0), index(1)`, `depth(any)`).
    pub attr: String,
    /// The colour location, `None` for the depth.
    pub color: Option<u32>,
}

/// The stage IO of a shader.
#[derive(Debug, Default)]
pub struct Plan {
    /// This program consumes the explicit domain draw ABI, not hardware vertex input.
    pub(super) domain: bool,
    /// The members of `main0_in`.
    pub in_members: Vec<String>,
    /// The members of `main0_out`.
    pub out_members: Vec<String>,
    /// The built-in entry parameters.
    pub builtins: BTreeSet<Builtin>,
    /// The user varyings a pixel shader reads.
    pub varyings: Vec<Varying>,
    /// The vertex attributes: location and MSL type.
    pub attributes: Vec<(u32, String)>,
    /// The user outputs of a vertex shader that start at `(0, 0, 0, 1)`.
    pub vec4_outputs: Vec<String>,
    /// The fragment colour outputs.
    pub colors: Vec<u32>,
    /// The members of the fragment output struct (colours, pads, depth).
    pub frag_outs: Vec<FragOut>,
    /// The fragment shader reads a primitive id.
    pub primitive_id: bool,
    /// The fragment shader writes a sample mask (the IR has no such output, so this stays false).
    pub sample_mask: bool,
    /// The vertex shader writes a layer.
    pub layer: bool,
    /// The shader writes a viewport index.
    pub viewport: bool,
    /// The shader writes a depth.
    pub depth: bool,
    /// The lane id is the constant zero (the visibility counter).
    pub lane_zero: bool,
    /// The locations of the user outputs of a vertex shader, in order.
    pub user_locs: Vec<u32>,
    /// The outputs a vertex shader adds for the inputs of its paired pixel shaders.
    pub pads: Vec<super::variant::vertex_pad::Pad>,
    /// The locations that a register indexes in the input, one member of `main0_in` each.
    pub ia: Vec<i32>,
    /// The locations that a register indexes in the output, one member of `main0_out` each.
    pub oa: Vec<i32>,
    /// The vertex shader writes a viewport mask, which becomes the viewport index.
    pub viewport_mask: bool,
    /// Facts about the plan that the reflection reports as notes.
    pub notes: Vec<String>,
}

impl Plan {
    /// The text of `main0_in`, or empty.
    pub fn in_struct(&self) -> String {
        Self::strukt("main0_in", &self.in_members)
    }

    /// The text of `main0_out`, or empty.
    pub fn out_struct(&self) -> String {
        Self::strukt("main0_out", &self.out_members)
    }

    fn strukt(name: &str, m: &[String]) -> String {
        if m.is_empty() {
            return String::new();
        }
        let body: String = m.iter().map(|l| format!("    {l}\n")).collect();
        format!("struct {name}\n{{\n{body}}};\n")
    }

    /// The expression of a whole input variable and its storage type.
    pub fn input(&self, v: IoVariable, loc: i32) -> R<(String, AggType)> {
        let (s, t): (String, AggType) = match v {
            IoVariable::UserDefined => (format!("in.loc{loc}"), AggType::VECTOR4 | AggType::FP32),
            IoVariable::FragmentCoord => {
                ("in.gl_FragCoord".into(), AggType::VECTOR4 | AggType::FP32)
            }
            IoVariable::FrontFacing => ("in.gl_FrontFacing".into(), AggType::BOOL),
            IoVariable::SampleId => ("in.gl_SampleID".into(), AggType::U32),
            IoVariable::PrimitiveId => ("in.gl_PrimitiveID".into(), AggType::U32),
            IoVariable::PointCoord => ("in.gl_PointCoord".into(), AggType::VECTOR2 | AggType::FP32),
            IoVariable::Layer => ("in.gl_Layer".into(), AggType::U32),
            IoVariable::ViewportIndex => ("in.gl_ViewportIndex".into(), AggType::U32),
            IoVariable::ThreadKill | IoVariable::HelperInvocation => {
                ("simd_is_helper_thread()".into(), AggType::BOOL)
            }
            IoVariable::VertexIndex => ("gl_VertexIndex".into(), AggType::U32),
            IoVariable::InstanceIndex => ("gl_InstanceIndex".into(), AggType::U32),
            IoVariable::VertexId => ("(gl_VertexIndex - gl_BaseVertex)".into(), AggType::U32),
            IoVariable::InstanceId => ("(gl_InstanceIndex - gl_BaseInstance)".into(), AggType::U32),
            IoVariable::DrawIndex => ("0u".into(), AggType::U32),
            IoVariable::BaseVertex => ("gl_BaseVertex".into(), AggType::U32),
            IoVariable::BaseInstance => ("gl_BaseInstance".into(), AggType::U32),
            IoVariable::ThreadId => (
                "gl_LocalInvocationID".into(),
                AggType::VECTOR3 | AggType::U32,
            ),
            IoVariable::CtaId => ("gl_WorkGroupID".into(), AggType::VECTOR3 | AggType::U32),
            IoVariable::GlobalId => (
                "gl_GlobalInvocationID".into(),
                AggType::VECTOR3 | AggType::U32,
            ),
            IoVariable::SubgroupLaneId => (self.lane(), AggType::U32),
            IoVariable::SubgroupEqMask
            | IoVariable::SubgroupGeMask
            | IoVariable::SubgroupGtMask
            | IoVariable::SubgroupLeMask
            | IoVariable::SubgroupLtMask => (self.lane_mask(v), AggType::VECTOR4 | AggType::U32),
            other => return Err(format!("unported: input variable {}", other.0)),
        };
        Ok((s, t))
    }

    /// The lines before the return of a vertex entry: the depth convention.
    pub fn vertex_epilogue(&self, depth_fix: bool) -> Vec<String> {
        let mut l = Vec::new();
        if depth_fix {
            l.push(
                "out.gl_Position.z = fma(out.gl_Position.z, 0.5, out.gl_Position.w * 0.5);".into(),
            );
        }
        l
    }

    /// The lane id expression.
    pub fn lane(&self) -> String {
        if self.lane_zero {
            "0u".into()
        } else {
            LANE.name.into()
        }
    }

    fn lane_mask(&self, v: IoVariable) -> String {
        let l = self.lane();
        let w = match v {
            IoVariable::SubgroupEqMask => format!("1u << {l}"),
            IoVariable::SubgroupGeMask => format!("0xffffffffu << {l}"),
            IoVariable::SubgroupGtMask => {
                format!("({l} >= 31u ? 0u : 0xffffffffu << ({l} + 1u))")
            }
            IoVariable::SubgroupLeMask => {
                format!("({l} >= 31u ? 0xffffffffu : (1u << ({l} + 1u)) - 1u)")
            }
            _ => format!("(1u << {l}) - 1u"),
        };
        format!("uint4({w}, 0u, 0u, 0u)")
    }

    /// The expression of a whole output variable and its storage type.
    pub fn output(&self, v: IoVariable, loc: i32, color: AggType) -> R<(String, AggType)> {
        let f4 = AggType::VECTOR4 | AggType::FP32;
        Ok(match v {
            IoVariable::UserDefined => (format!("out.loc{loc}"), f4),
            IoVariable::FragmentOutputColor => (format!("out.c{loc}"), color),
            IoVariable::Position => ("out.gl_Position".into(), f4),
            IoVariable::PointSize => ("out.gl_PointSize".into(), AggType::FP32),
            IoVariable::ClipDistance => ("out.gl_ClipDistance".into(), AggType::FP32),
            IoVariable::FragmentOutputDepth => ("out.gl_FragDepth".into(), AggType::FP32),
            IoVariable::Layer => ("out.gl_Layer".into(), AggType::U32),
            IoVariable::ViewportIndex => ("out.gl_ViewportIndex".into(), AggType::U32),
            IoVariable::ViewportMask => ("out.gl_ViewportIndex".into(), AggType::U32),
            other => return Err(format!("unported: output variable {}", other.0)),
        })
    }
}

/// The IR type of a built-in variable.
pub fn built_in_type(v: IoVariable) -> R<AggType> {
    let f4 = AggType::VECTOR4 | AggType::FP32;
    let u3 = AggType::VECTOR3 | AggType::U32;
    Ok(match v {
        IoVariable::BaseInstance
        | IoVariable::BaseVertex
        | IoVariable::DrawIndex
        | IoVariable::InstanceId
        | IoVariable::InstanceIndex
        | IoVariable::InvocationId
        | IoVariable::Layer
        | IoVariable::PrimitiveId
        | IoVariable::SampleId
        | IoVariable::VertexId
        | IoVariable::VertexIndex
        | IoVariable::ViewportIndex
        | IoVariable::ViewportMask => AggType::S32,
        IoVariable::FragmentCoord | IoVariable::Position => f4,
        IoVariable::PointCoord => AggType::VECTOR2 | AggType::FP32,
        IoVariable::PointSize | IoVariable::FragmentOutputDepth => AggType::FP32,
        IoVariable::ClipDistance => AggType::FP32,
        IoVariable::FrontFacing | IoVariable::ThreadKill | IoVariable::HelperInvocation => {
            AggType::BOOL
        }
        IoVariable::CtaId | IoVariable::GlobalId | IoVariable::ThreadId => u3,
        IoVariable::SubgroupLaneId => AggType::U32,
        IoVariable::SubgroupEqMask
        | IoVariable::SubgroupGeMask
        | IoVariable::SubgroupGtMask
        | IoVariable::SubgroupLeMask
        | IoVariable::SubgroupLtMask => AggType::VECTOR4 | AggType::U32,
        other => return Err(format!("unported: built-in variable {}", other.0)),
    })
}

fn io_ty(t: AggType) -> R<String> {
    msl_type(t)
}

/// Plan the IO of a program.
pub fn build(
    info: &StructInfo,
    props: &Props,
    shader: Shader,
    lane_used: bool,
    base_instance: bool,
    lane_zero: bool,
) -> R<Plan> {
    let d = &props.definitions;
    let mut p = Plan {
        lane_zero,
        domain: props.definitions.stage == Some(crate::attr::Stage::TessellationEvaluation),
        ..Plan::default()
    };
    let mut seen = BTreeSet::new();
    let first_color = if d.dual_source_blend {
        info.io_definitions
            .iter()
            .filter(|i| i.io_variable == IoVariable::FragmentOutputColor)
            .map(|i| i.location)
            .min()
            .unwrap_or(0)
    } else {
        0
    };
    let mut users = Vec::new();
    for io in &info.io_definitions {
        if !seen.insert((io.storage_kind.0, io.io_variable.0, io.location)) {
            continue;
        }
        if p.domain
            && matches!(
                io.storage_kind,
                StorageKind::Input | StorageKind::InputPerPatch
            )
        {
            super::domain::check_input(io.storage_kind, io.io_variable)?;
            continue;
        }
        let out = matches!(io.storage_kind, StorageKind::Output);
        if !out && !matches!(io.storage_kind, StorageKind::Input) {
            return Err("unported: per-patch input or output".into());
        }
        let (v, l) = (io.io_variable, io.location);
        if out {
            output(&mut p, props, shader, v, l, first_color, &mut users)?;
        } else {
            input(&mut p, props, shader, v, l)?;
        }
    }
    users.sort_unstable();
    users.dedup();
    for l in &users {
        p.out_members
            .push(format!("float4 loc{l} [[user(locn{l})]];"));
        p.vec4_outputs.push(format!("loc{l}"));
        p.user_locs.push(*l as u32);
    }
    if shader == Shader::Vertex {
        let pos = if props.host.reduced_precision {
            "[[position, invariant]]"
        } else {
            "[[position]]"
        };
        p.out_members
            .insert(0, format!("float4 gl_Position {pos};"));
        if p.layer {
            p.out_members.retain(|m| !m.contains("[[point_size]]"));
        }
        if base_instance {
            p.builtins.insert(BASE_INSTANCE);
        }
    }
    if lane_used && !lane_zero {
        if shader == Shader::Vertex {
            return Err("unported: subgroup lane id in a vertex shader".into());
        }
        p.builtins.insert(LANE);
    }
    if shader == Shader::Fragment && !d.dual_source_blend {
        let top = p.colors.iter().copied().max();
        for l in 0..top.map_or(0, |t| t + 1) {
            if !p.colors.contains(&l) {
                p.out_members
                    .push(format!("float4 c{l}_pad [[color({l})]];"));
                p.frag_outs.push(FragOut {
                    name: format!("c{l}_pad"),
                    ty: "float4".into(),
                    attr: format!("color({l})"),
                    color: Some(l),
                });
            }
        }
        if let Some(t) = top {
            p.colors = (0..=t).collect();
        }
    }
    ioidx::finish(&mut p);
    p.out_members.sort_by_key(|m| m.contains("[[color"));
    p.frag_outs.sort_by_key(|m| m.color.is_some());
    Ok(p)
}

fn builtin(p: &mut Plan, shader: Shader, v: IoVariable) -> R<()> {
    let b: &[Builtin] = match (shader, v) {
        (Shader::Vertex, IoVariable::VertexIndex) => &[VERTEX_ID],
        (Shader::Vertex, IoVariable::InstanceIndex) => &[INSTANCE_ID],
        (Shader::Vertex, IoVariable::VertexId) => &[VERTEX_ID, BASE_VERTEX],
        (Shader::Vertex, IoVariable::InstanceId) => &[INSTANCE_ID, BASE_INSTANCE],
        (Shader::Vertex, IoVariable::BaseVertex) => &[BASE_VERTEX],
        (Shader::Vertex, IoVariable::BaseInstance) => &[BASE_INSTANCE],
        (Shader::Vertex, IoVariable::DrawIndex) => {
            ioidx::draw_index(p);
            return Ok(());
        }
        (Shader::Compute, IoVariable::ThreadId) => &[Builtin {
            name: "gl_LocalInvocationID",
            ty: "uint3",
            attr: "thread_position_in_threadgroup",
        }],
        (Shader::Compute, IoVariable::CtaId) => &[Builtin {
            name: "gl_WorkGroupID",
            ty: "uint3",
            attr: "threadgroup_position_in_grid",
        }],
        (Shader::Compute, IoVariable::GlobalId) => &[Builtin {
            name: "gl_GlobalInvocationID",
            ty: "uint3",
            attr: "thread_position_in_grid",
        }],
        (Shader::Fragment | Shader::Compute, IoVariable::SubgroupLaneId)
        | (Shader::Fragment | Shader::Compute, IoVariable::SubgroupEqMask)
        | (Shader::Fragment | Shader::Compute, IoVariable::SubgroupGeMask)
        | (Shader::Fragment | Shader::Compute, IoVariable::SubgroupGtMask)
        | (Shader::Fragment | Shader::Compute, IoVariable::SubgroupLeMask)
        | (Shader::Fragment | Shader::Compute, IoVariable::SubgroupLtMask) => {
            if !p.lane_zero {
                p.builtins.insert(LANE);
            }
            return Ok(());
        }
        (Shader::Vertex, IoVariable::SubgroupLaneId) if p.lane_zero => return Ok(()),
        _ => return Err(format!("unported: input variable {} in this stage", v.0)),
    };
    p.builtins.extend(b.iter().copied());
    Ok(())
}

/// The components of an input that its map reads, one bit each. A map that names none counts as all four.
fn read_mask(imap: &[u8; 4]) -> u8 {
    let m = (0..4).filter(|c| imap[*c] != 0).fold(0, |m, c| m | 1 << c);
    if m == 0 { 0xf } else { m }
}

/// The member of a pixel shader input at a location, with the interpolation its map names.
pub(super) fn varying(p: &mut Plan, imap: &[u8; 4], l: i32) {
    let iq = imap.iter().copied().find(|x| *x != 0).unwrap_or(imap[3]);
    let (attr, interp) = match iq {
        1 => (", flat", "flat"),
        3 => (", center_no_perspective", "center_no_perspective"),
        _ => ("", ""),
    };
    p.in_members
        .push(format!("float4 loc{l} [[user(locn{l}){attr}]];"));
    p.varyings.push(Varying {
        location: l as u32,
        msl_type: "float4".into(),
        interpolation: interp.into(),
        read: read_mask(imap),
    });
}

fn input(p: &mut Plan, props: &Props, shader: Shader, v: IoVariable, l: i32) -> R<()> {
    let d = &props.definitions;
    match (shader, v) {
        (Shader::Vertex | Shader::Fragment, IoVariable::UserDefined) if d.ia_indexing => {
            ioidx::input_members(p, d, shader)?
        }
        (Shader::Vertex, IoVariable::UserDefined) => {
            let t = io_ty(d.user_defined_type(l, false)?)?;
            p.in_members.push(format!("{t} loc{l} [[attribute({l})]];"));
            p.attributes.push((l as u32, t));
        }
        (Shader::Fragment, IoVariable::UserDefined) => {
            let imap = d.imap_types.get(l as usize).ok_or("imap type missing")?;
            varying(p, imap, l);
        }
        (Shader::Fragment, IoVariable::FragmentCoord) => p
            .in_members
            .push("float4 gl_FragCoord [[position]];".into()),
        (Shader::Fragment, IoVariable::FrontFacing) => p
            .in_members
            .push("bool gl_FrontFacing [[front_facing]];".into()),
        (Shader::Fragment, IoVariable::SampleId) => {
            p.in_members.push("uint gl_SampleID [[sample_id]];".into())
        }
        (Shader::Fragment, IoVariable::PrimitiveId) => {
            p.primitive_id = true;
            p.in_members
                .push("uint gl_PrimitiveID [[primitive_id]];".into());
        }
        (Shader::Fragment, IoVariable::PointCoord) => p
            .in_members
            .push("float2 gl_PointCoord [[point_coord]];".into()),
        (Shader::Fragment, IoVariable::Layer) => p
            .in_members
            .push("uint gl_Layer [[render_target_array_index]];".into()),
        (Shader::Fragment, IoVariable::ViewportIndex) => p
            .in_members
            .push("uint gl_ViewportIndex [[viewport_array_index]];".into()),
        (Shader::Fragment, IoVariable::ThreadKill | IoVariable::HelperInvocation) => {}
        _ => builtin(p, shader, v)?,
    }
    Ok(())
}

fn output(
    p: &mut Plan,
    props: &Props,
    shader: Shader,
    v: IoVariable,
    l: i32,
    first_color: i32,
    users: &mut Vec<i32>,
) -> R<()> {
    let d = &props.definitions;
    match (shader, v) {
        (Shader::Vertex, IoVariable::UserDefined) if d.oa_indexing => {
            ioidx::output_members(p, users)
        }
        (Shader::Vertex, IoVariable::UserDefined) => users.push(l),
        (Shader::Vertex, IoVariable::Position) => {}
        (Shader::Vertex, IoVariable::PointSize) => p
            .out_members
            .push("float gl_PointSize [[point_size]];".into()),
        (Shader::Vertex, IoVariable::ClipDistance) => p
            .out_members
            .push("float gl_ClipDistance [[clip_distance]] [8];".into()),
        (Shader::Vertex, IoVariable::Layer) => {
            p.layer = true;
            p.out_members
                .push("uint gl_Layer [[render_target_array_index]];".into());
        }
        (Shader::Vertex, IoVariable::ViewportIndex) => {
            p.viewport = true;
            p.out_members
                .push("uint gl_ViewportIndex [[viewport_array_index]];".into());
        }
        (Shader::Fragment, IoVariable::FragmentOutputColor) => {
            let t = io_ty(d.fragment_output_color_type(l)?)?;
            let attr = if d.dual_source_blend && ((l - first_color) as u32) < 2 {
                format!("color({first_color}), index({})", l - first_color)
            } else {
                format!("color({l})")
            };
            p.out_members.push(format!("{t} c{l} [[{attr}]];"));
            p.frag_outs.push(FragOut {
                name: format!("c{l}"),
                ty: t,
                attr,
                color: Some(l as u32),
            });
            p.colors.push(l as u32);
        }
        (Shader::Fragment, IoVariable::FragmentOutputDepth) => {
            p.depth = true;
            p.out_members
                .push("float gl_FragDepth [[depth(any)]];".into());
            p.frag_outs.push(FragOut {
                name: "gl_FragDepth".into(),
                ty: "float".into(),
                attr: "depth(any)".into(),
                color: None,
            });
        }
        (Shader::Vertex, IoVariable::ViewportMask) => ioidx::viewport_mask(p),
        _ => return Err(format!("unported: output variable {} in this stage", v.0)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_masks_use_the_lane_id() {
        let p = Plan::default();
        let (s, t) = p.input(IoVariable::SubgroupLtMask, 0).unwrap();
        assert!(s.contains("gl_SubgroupInvocationID"));
        assert_eq!(t, AggType::VECTOR4 | AggType::U32);
        let z = Plan {
            lane_zero: true,
            ..Plan::default()
        };
        assert_eq!(z.input(IoVariable::SubgroupLaneId, 0).unwrap().0, "0u");
    }

    #[test]
    fn a_varying_reads_the_components_its_map_names() {
        let mut p = Plan::default();
        varying(&mut p, &[3, 3, 3, 0], 4);
        varying(&mut p, &[0, 2, 0, 2], 5);
        varying(&mut p, &[0; 4], 6);
        let read: Vec<u8> = p.varyings.iter().map(|v| v.read).collect();
        assert_eq!(read, vec![0x7, 0xa, 0xf]);
    }

    #[test]
    fn structs_are_empty_without_members() {
        assert_eq!(Plan::default().in_struct(), "");
        let p = Plan {
            out_members: vec!["float4 gl_Position [[position]];".into()],
            ..Plan::default()
        };
        assert!(p.out_struct().starts_with("struct main0_out\n{\n"));
    }
}
