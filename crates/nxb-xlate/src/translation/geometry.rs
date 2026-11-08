//! The geometry shader front end of the compute emulation: `Translate(asCompute: true)` of a geometry shader and
//! `GenerateVertexPassthroughForCompute`, up to the structured program. The back ends consume the result.

use super::context::TranslatorContext;
use super::core::{StructRes, buf_info, definitions_for, host_caps, properties_of, tex_info};
use super::pipeline::{Transformed, run_to_xform_mode};
use super::props::{BufferDefinition, BufferLayout, ShaderStage};
use super::resources::ResourceManager;
use super::vac::{compute_definitions, float_array_struct, get_resource_reservations};
use crate::accessor::Accessor;
use crate::attr::{Stage, feature};
use crate::driver::Program;
use crate::ir::{Inst, IoVariable, Ir, StorageKind};
use crate::props::{self as cp, Props};
use crate::structured::StructInfo;
use crate::structured::program::{FunctionIn, make_structured_program};
use crate::translation::resources_res::ResourceReservations;
use crate::xform::helper::B;

/// The compute emulation of a geometry shader (`Translate(asCompute: true)`): the structured program of the compute
/// shader with the definitions of a 32 by 32 (or 32 by 1 with a fixed input) work group, the properties of the
/// reserved buffers and `geometry_stride`, the size in 32 bit words of one output vertex.
pub fn structure_geometry_compute(
    tc: &TranslatorContext,
    acc: &mut Accessor,
    debug_mode: bool,
) -> Result<(StructInfo, Props, Program), String> {
    if tc.stage() != Stage::Geometry {
        let kind = match tc.stage() {
            Stage::Vertex => "a vertex shader",
            Stage::TessellationControl => "a tessellation control shader",
            Stage::TessellationEvaluation => "a tessellation evaluation shader",
            Stage::Fragment => "a fragment shader",
            Stage::Compute => "a compute shader",
            Stage::Geometry => "a geometry shader",
        };
        return Err(format!(
            "unported: vertex as compute of {kind}, only geometry shaders run as compute"
        ));
    }
    let mut t = run_to_xform_mode(tc, acc, true)?;
    let defs = definitions_for(&compute_definitions(tc, acc));
    let info = structure_functions(&mut t, acc, &defs, debug_mode)?;
    let features = t.used_features as i32 | tc.program.used_features;
    let stride =
        t.rm.reservations
            .as_ref()
            .map_or(0, |r| r.output_size_per_invocation);
    let program = Program {
        cbufs: buf_info(&t.rm, false),
        sbufs: buf_info(&t.rm, true),
        textures: tex_info(t.rm.get_texture_descriptors(true)),
        images: tex_info(t.rm.get_image_descriptors(true)),
        uses_frag_coord: features & feature::FRAG_COORD_XY != 0,
        scaled: Vec::new(),
        frag_out_map: tc.definitions.omap_targets as i64,
        geometry_stride: stride as i64,
    };
    let props = Props {
        definitions: defs,
        host: host_caps(acc),
        properties: properties_of(&t.rm),
        passthrough_attributes: 0,
        target_api: 1,
        cb_slot_to_binding: Vec::new(),
        helper_functions_mask: 0,
        io_definitions: Vec::new(),
    };
    Ok((info, props, program))
}

fn structure_functions(
    t: &mut Transformed,
    acc: &mut Accessor,
    defs: &cp::Definitions,
    debug_mode: bool,
) -> Result<StructInfo, String> {
    let mut user = std::mem::take(&mut t.funcs);
    let mut helpers: Vec<crate::xform::helper::Function> = t
        .hfm
        .functions
        .iter_mut()
        .skip(user.len())
        .filter_map(Option::take)
        .collect();
    let mut ins = Vec::new();
    for (i, f) in user.iter_mut().enumerate() {
        ins.push(FunctionIn {
            name: format!("fun{i}"),
            blocks: &mut f.blocks,
            in_arguments_count: f.in_arguments,
            out_arguments_count: f.out_arguments,
            returns_value: false,
        });
    }
    for h in helpers.iter_mut() {
        ins.push(FunctionIn {
            name: h.name.clone(),
            blocks: &mut h.blocks,
            in_arguments_count: h.in_arguments as usize,
            out_arguments_count: h.out_arguments as usize,
            returns_value: h.returns_value,
        });
    }
    let mut res = StructRes { rm: &mut t.rm, acc };
    make_structured_program(&mut t.ir, &mut ins, defs, &mut res, 0, debug_mode)
}

/// `GenerateVertexPassthroughForCompute`: the vertex shader that draws the vertices a geometry compute shader wrote.
/// It reads the vertex of the draw from the `vb_input` buffer and copies every output the geometry shader writes.
pub fn structure_geometry_reader(
    tc: &TranslatorContext,
    acc: &mut Accessor,
    debug_mode: bool,
) -> Result<(StructInfo, Props), String> {
    let reservations: ResourceReservations = get_resource_reservations(tc, acc);
    let mut rm = ResourceManager::new(ShaderStage::Vertex, None);
    let data_binding = reservations.vertex_output_storage_buffer_binding;
    rm.properties
        .add_or_update_storage_buffer(BufferDefinition {
            layout: BufferLayout::Std430,
            set: 1,
            binding: data_binding,
            name: "vb_input".to_string(),
            ty: float_array_struct(),
        });
    let mut ir = Ir::default();
    let mut b = B::new(&mut ir);
    let vi = b.ir.const_(IoVariable::VertexIndex.0 as i32);
    let vertex_index = b.op_kind(Inst::Load, StorageKind::Input, &[vi]);
    let stride = b.ir.const_(reservations.output_size_per_invocation);
    let base = b.op(Inst::Multiply, &[vertex_index, stride]);
    for (def, offset) in reservations.output_offsets() {
        let vertex_offset = if offset != 0 {
            let k = b.ir.const_(offset);
            b.op(Inst::Add, &[base, k])
        } else {
            base
        };
        let binding = b.ir.const_(data_binding);
        let zero = b.ir.const_(0);
        let value = b.op_kind(
            Inst::Load,
            StorageKind::StorageBuffer,
            &[binding, zero, vertex_offset],
        );
        let io = b.ir.const_(def.io_variable.0 as i32);
        let mut srcs = vec![io];
        if def.io_variable == IoVariable::UserDefined {
            srcs.push(b.ir.const_(def.location));
            srcs.push(b.ir.const_(def.component));
        } else if ResourceReservations::is_vector_or_array_variable(def.io_variable) {
            srcs.push(b.ir.const_(def.component));
        }
        srcs.push(value);
        b.raw(Inst::Store, StorageKind::Output, None, &srcs);
    }
    let function = b.finish_with("main", false, 0, 0)?;
    let defs = cp::Definitions {
        stage: Some(Stage::Vertex),
        last_in_vertex_pipeline: true,
        origin_upper_left: true,
        tess_patch_type: 1,
        ..Default::default()
    };
    let mut blocks = function.blocks;
    let mut ins = vec![FunctionIn {
        name: function.name,
        blocks: &mut blocks,
        in_arguments_count: 0,
        out_arguments_count: 0,
        returns_value: false,
    }];
    let info = {
        let mut res = StructRes { rm: &mut rm, acc };
        make_structured_program(&mut ir, &mut ins, &defs, &mut res, 0, debug_mode)?
    };
    let props = Props {
        definitions: defs,
        host: host_caps(acc),
        properties: properties_of(&rm),
        passthrough_attributes: 0,
        target_api: 1,
        cb_slot_to_binding: Vec::new(),
        helper_functions_mask: 0,
        io_definitions: Vec::new(),
    };
    Ok((info, props))
}
