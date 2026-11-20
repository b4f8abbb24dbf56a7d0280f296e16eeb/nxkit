//! `AttributeMap` (`Instructions/AttributeMap.cs`): attribute offsets to IO variables, and the attribute load/store.

use crate::attr::Stage;
use crate::emit::emitter::EmitterContext;
use crate::ir::inst::{IoVariable, StorageKind};
use crate::ir::ops::Opd;
use crate::translation::definitions::Definitions;
use std::collections::HashMap;
use std::sync::OnceLock;

/// `StagesMask`: bit `1 << stage`.
mod mask {
    pub(super) const NONE: u8 = 0;
    pub(super) const VERTEX: u8 = 1 << 1;
    pub(super) const TESS_CONTROL: u8 = 1 << 2;
    pub(super) const TESS_EVAL: u8 = 1 << 3;
    pub(super) const GEOMETRY: u8 = 1 << 4;
    pub(super) const FRAGMENT: u8 = 1 << 5;
    pub(super) const TESSELLATION: u8 = TESS_CONTROL | TESS_EVAL;
    pub(super) const VERTEX_TESSELLATION_GEOMETRY: u8 = VERTEX | TESSELLATION | GEOMETRY;
    pub(super) const TESSELLATION_GEOMETRY_FRAGMENT: u8 = TESSELLATION | GEOMETRY | FRAGMENT;
    pub(super) const ALL_GRAPHICS: u8 = VERTEX | TESSELLATION | GEOMETRY | FRAGMENT;
}

/// The part of `AggregateType` the map needs: the element count and the array flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agg {
    pub count: i32,
    pub array: bool,
}

const SCALAR: Agg = Agg {
    count: 1,
    array: false,
};
const VEC2: Agg = Agg {
    count: 2,
    array: false,
};
const VEC4: Agg = Agg {
    count: 4,
    array: false,
};
const ARRAY: Agg = Agg {
    count: 1,
    array: true,
};

#[derive(Clone, Copy)]
struct Entry {
    base_offset: i32,
    ty: Agg,
    io: IoVariable,
    input_mask: u8,
    output_mask: u8,
}

type Map = HashMap<i32, Entry>;

#[allow(clippy::too_many_arguments)]
fn add(
    map: &mut Map,
    offset: i32,
    ty: Agg,
    io: IoVariable,
    input_mask: u8,
    output_mask: u8,
    count: i32,
    upper_bound: i32,
) {
    let base_offset = offset;
    let mut offset = offset;
    for _ in 0..count {
        for _ in 0..ty.count {
            map.insert(
                offset,
                Entry {
                    base_offset,
                    ty,
                    io,
                    input_mask,
                    output_mask,
                },
            );
            offset += 4;
            if offset >= upper_bound {
                return;
            }
        }
    }
}

fn create_map() -> Map {
    use mask::*;
    let mut m = Map::new();
    let a = &mut m;
    let tgf = TESSELLATION_GEOMETRY_FRAGMENT;
    let vtg = VERTEX_TESSELLATION_GEOMETRY;
    add(
        a,
        0x060,
        SCALAR,
        IoVariable::PrimitiveId,
        tgf,
        GEOMETRY,
        1,
        0x400,
    );
    add(a, 0x064, SCALAR, IoVariable::Layer, FRAGMENT, vtg, 1, 0x400);
    add(
        a,
        0x068,
        SCALAR,
        IoVariable::ViewportIndex,
        FRAGMENT,
        vtg,
        1,
        0x400,
    );
    add(a, 0x06c, SCALAR, IoVariable::PointSize, NONE, vtg, 1, 0x400);
    add(a, 0x070, VEC4, IoVariable::Position, tgf, vtg, 1, 0x400);
    add(
        a,
        0x080,
        VEC4,
        IoVariable::UserDefined,
        ALL_GRAPHICS,
        vtg,
        32,
        0x400,
    );
    add(
        a,
        0x280,
        VEC4,
        IoVariable::FrontColorDiffuse,
        tgf,
        vtg,
        1,
        0x400,
    );
    add(
        a,
        0x290,
        VEC4,
        IoVariable::FrontColorSpecular,
        tgf,
        vtg,
        1,
        0x400,
    );
    add(
        a,
        0x2a0,
        VEC4,
        IoVariable::BackColorDiffuse,
        tgf,
        vtg,
        1,
        0x400,
    );
    add(
        a,
        0x2b0,
        VEC4,
        IoVariable::BackColorSpecular,
        tgf,
        vtg,
        1,
        0x400,
    );
    add(
        a,
        0x2c0,
        ARRAY,
        IoVariable::ClipDistance,
        tgf,
        vtg,
        8,
        0x400,
    );
    add(
        a,
        0x2e0,
        VEC2,
        IoVariable::PointCoord,
        FRAGMENT,
        NONE,
        1,
        0x400,
    );
    add(a, 0x2e8, SCALAR, IoVariable::FogCoord, tgf, vtg, 1, 0x400);
    add(
        a,
        0x2f0,
        VEC2,
        IoVariable::TessellationCoord,
        TESS_EVAL,
        NONE,
        1,
        0x400,
    );
    add(
        a,
        0x2f8,
        SCALAR,
        IoVariable::InstanceId,
        VERTEX,
        NONE,
        1,
        0x400,
    );
    add(
        a,
        0x2fc,
        SCALAR,
        IoVariable::VertexId,
        VERTEX,
        NONE,
        1,
        0x400,
    );
    add(a, 0x300, VEC4, IoVariable::TextureCoord, tgf, vtg, 1, 0x400);
    add(
        a,
        0x3a0,
        ARRAY,
        IoVariable::ViewportMask,
        FRAGMENT,
        vtg,
        1,
        0x400,
    );
    add(
        a,
        0x3fc,
        SCALAR,
        IoVariable::FrontFacing,
        FRAGMENT,
        NONE,
        1,
        0x400,
    );
    m
}

fn create_per_patch_map() -> Map {
    use mask::*;
    let mut m = Map::new();
    let a = &mut m;
    add(
        a,
        0x000,
        VEC4,
        IoVariable::TessellationLevelOuter,
        TESS_EVAL,
        TESS_CONTROL,
        1,
        0x400,
    );
    add(
        a,
        0x010,
        VEC2,
        IoVariable::TessellationLevelInner,
        TESS_EVAL,
        TESS_CONTROL,
        1,
        0x400,
    );
    add(
        a,
        0x018,
        VEC4,
        IoVariable::UserDefined,
        TESS_EVAL,
        TESS_CONTROL,
        31,
        0x200,
    );
    m
}

fn entry(offset: i32, per_patch: bool) -> Option<Entry> {
    static MAPS: OnceLock<(Map, Map)> = OnceLock::new();
    let (m, p) = MAPS.get_or_init(|| (create_map(), create_per_patch_map()));
    if per_patch {
        p.get(&offset).copied()
    } else {
        m.get(&offset).copied()
    }
}

fn stage_bit(stage: Stage) -> u8 {
    1 << (stage as u32)
}

/// `Enum.ToString()` of an `IoVariable`.
pub fn io_name(io: IoVariable) -> &'static str {
    const NAMES: [&str; 44] = [
        "Invalid",
        "BackColorDiffuse",
        "BackColorSpecular",
        "BaseInstance",
        "BaseVertex",
        "ClipDistance",
        "CtaId",
        "DrawIndex",
        "FogCoord",
        "FragmentCoord",
        "FragmentOutputColor",
        "FragmentOutputDepth",
        "FrontColorDiffuse",
        "FrontColorSpecular",
        "FrontFacing",
        "GlobalId",
        "InstanceId",
        "InstanceIndex",
        "InvocationId",
        "Layer",
        "PatchVertices",
        "PointCoord",
        "PointSize",
        "Position",
        "PrimitiveId",
        "SampleId",
        "SubgroupEqMask",
        "SubgroupGeMask",
        "SubgroupGtMask",
        "SubgroupLaneId",
        "SubgroupLeMask",
        "SubgroupLtMask",
        "TessellationCoord",
        "TessellationLevelInner",
        "TessellationLevelOuter",
        "TextureCoord",
        "ThreadId",
        "ThreadKill",
        "UserDefined",
        "VertexId",
        "VertexIndex",
        "ViewportIndex",
        "ViewportMask",
        "HelperInvocation",
    ];
    NAMES.get(io.0 as usize).copied().unwrap_or("Invalid")
}

fn get_io_variable_of(stage: Stage, e: &Entry) -> IoVariable {
    if e.io == IoVariable::Position && stage == Stage::Fragment {
        IoVariable::FragmentCoord
    } else {
        e.io
    }
}

/// `ShaderDefinitions.GetUserDefinedType` (element type is not needed here).
pub fn user_defined_type(d: &Definitions, is_output: bool) -> Agg {
    if (!is_output && d.ia_indexing) || (is_output && d.oa_indexing) {
        return Agg {
            count: 4,
            array: true,
        };
    }
    VEC4
}

fn get_type(d: &Definitions, is_output: bool, e: &Entry) -> Agg {
    if e.io == IoVariable::UserDefined {
        user_defined_type(d, is_output)
    } else {
        e.ty
    }
}

fn has_per_location(d: &Definitions, io: IoVariable, is_output: bool) -> bool {
    d.has_per_location_input_or_output(
        io == IoVariable::UserDefined,
        io == IoVariable::FragmentOutputColor,
        is_output,
    )
}

/// `HasPrimitiveVertex`.
pub fn has_primitive_vertex(stage: Stage, is_output: bool) -> bool {
    if is_output {
        return false;
    }
    matches!(
        stage,
        Stage::TessellationControl | Stage::TessellationEvaluation | Stage::Geometry
    )
}

/// `HasInvocationId`.
pub fn has_invocation_id(stage: Stage, is_output: bool) -> bool {
    is_output && stage == Stage::TessellationControl
}

/// `IsSupportedByHost`: the job accessor supports viewport index and mask everywhere.
fn is_supported_by_host(_stage: Stage, _io: IoVariable) -> bool {
    true
}

/// `GetIoVariable(definitions, offset, out location)`: `Invalid` when the offset is no output of this stage.
pub fn get_io_variable(d: &Definitions, offset: i32) -> (IoVariable, i32) {
    let Some(e) = entry(offset, false) else {
        return (IoVariable::Invalid, 0);
    };
    if stage_bit(d.stage) & e.output_mask == 0 {
        return (IoVariable::Invalid, 0);
    }
    let mut location = 0;
    if has_per_location(d, e.io, true) {
        location = (offset - e.base_offset) / 16;
    }
    (get_io_variable_of(d.stage, &e), location)
}

/// `GenerateAttributeLoad`.
pub fn generate_attribute_load(
    ctx: &mut EmitterContext<'_, '_>,
    prim_vertex: Option<Opd>,
    offset: i32,
    is_output: bool,
    is_per_patch: bool,
) -> Opd {
    let stage = ctx.tc.definitions.stage;
    let Some(e) = entry(offset, is_per_patch) else {
        ctx.acc
            .log(&format!("Attribute offset 0x{offset:X} is not valid."));
        return ctx.ir.const_(0);
    };
    let valid = if is_output {
        e.output_mask
    } else {
        e.input_mask
    };
    if stage_bit(stage) & valid == 0 {
        ctx.acc.log(&format!(
            "Attribute offset 0x{:X} ({}) is not valid for stage {:?}.",
            offset,
            io_name(e.io),
            stage
        ));
        return ctx.ir.const_(0);
    }
    if !is_supported_by_host(stage, e.io) {
        ctx.acc.log(&format!(
            "Attribute offset 0x{:X} ({}) is not supported by the host for stage {:?}.",
            offset,
            io_name(e.io),
            stage
        ));
        return ctx.ir.const_(0);
    }
    let mut prim_vertex = prim_vertex;
    if has_invocation_id(stage, is_output) && !is_per_patch {
        prim_vertex = Some(ctx.load_io(StorageKind::Input, IoVariable::InvocationId, None));
    }
    let inner_index = (offset - e.base_offset) / 4;
    let kind = if is_per_patch {
        if is_output {
            StorageKind::OutputPerPatch
        } else {
            StorageKind::InputPerPatch
        }
    } else if is_output {
        StorageKind::Output
    } else {
        StorageKind::Input
    };
    let io = get_io_variable_of(stage, &e);
    let ty = get_type(&ctx.tc.definitions, is_output, &e);
    let is_array = ty.array;
    let has_array_index = is_array || has_per_location(&ctx.tc.definitions, io, is_output);
    let has_element_index = ty.count > 1;
    if has_array_index && has_element_index {
        let array_index = inner_index / ty.count;
        let element_index = inner_index - array_index * ty.count;
        let ai = ctx.ir.const_(array_index);
        let ei = ctx.ir.const_(element_index);
        match prim_vertex {
            Some(p) if !is_array => ctx.load_io_a(kind, io, Some(ai), p, ei),
            _ => ctx.load_io_a(kind, io, prim_vertex, ai, ei),
        }
    } else if has_array_index || has_element_index {
        let ii = ctx.ir.const_(inner_index);
        match prim_vertex {
            Some(p) if !is_array && has_array_index => ctx.load_io_e(kind, io, Some(ii), p),
            _ => ctx.load_io_e(kind, io, prim_vertex, ii),
        }
    } else {
        ctx.load_io(kind, io, prim_vertex)
    }
}

/// `GenerateAttributeStore`.
pub fn generate_attribute_store(
    ctx: &mut EmitterContext<'_, '_>,
    offset: i32,
    is_per_patch: bool,
    value: Opd,
) {
    let stage = ctx.tc.definitions.stage;
    let Some(e) = entry(offset, is_per_patch) else {
        ctx.acc
            .log(&format!("Attribute offset 0x{offset:X} is not valid."));
        return;
    };
    if stage_bit(stage) & e.output_mask == 0 {
        ctx.acc.log(&format!(
            "Attribute offset 0x{:X} ({}) is not valid for stage {:?}.",
            offset,
            io_name(e.io),
            stage
        ));
        return;
    }
    if !is_supported_by_host(stage, e.io) {
        ctx.acc.log(&format!(
            "Attribute offset 0x{:X} ({}) is not supported by the host for stage {:?}.",
            offset,
            io_name(e.io),
            stage
        ));
        return;
    }
    let mut invocation = None;
    if has_invocation_id(stage, true) && !is_per_patch {
        invocation = Some(ctx.load_io(StorageKind::Input, IoVariable::InvocationId, None));
    }
    let inner_index = (offset - e.base_offset) / 4;
    let kind = if is_per_patch {
        StorageKind::OutputPerPatch
    } else {
        StorageKind::Output
    };
    let io = get_io_variable_of(stage, &e);
    let ty = get_type(&ctx.tc.definitions, true, &e);
    let is_array = ty.array;
    let has_array_index = is_array || has_per_location(&ctx.tc.definitions, io, true);
    let has_element_index = ty.count > 1;
    if has_array_index && has_element_index {
        let array_index = inner_index / ty.count;
        let element_index = inner_index - array_index * ty.count;
        let ai = ctx.ir.const_(array_index);
        let ei = ctx.ir.const_(element_index);
        match invocation {
            Some(inv) if !is_array => ctx.store_io_a(kind, io, Some(ai), inv, ei, value),
            _ => ctx.store_io_a(kind, io, invocation, ai, ei, value),
        }
    } else if has_array_index || has_element_index {
        let ii = ctx.ir.const_(inner_index);
        match invocation {
            Some(inv) if !is_array && has_array_index => {
                ctx.store_io_e(kind, io, Some(ii), inv, value)
            }
            _ => ctx.store_io_e(kind, io, invocation, ii, value),
        }
    } else {
        ctx.store_io(kind, io, invocation, value);
    }
}
