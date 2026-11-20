//! Vertex-as-compute set-up of `TranslatorContext`: `GetResourceReservations`, the `CreateResourceManager` part for
//! `vertexAsCompute` and `GetDefinitions(vertexAsCompute)`. Only the geometry stage is built: the job driver never asks
//! for a vertex stage as compute.

use super::context::{TranslatorContext, set_current_memories};
use super::definitions::Definitions;
use super::props::{
    AggregateType, BufferDefinition, BufferLayout, ShaderStage, StructureField, StructureType,
    TextureDefinition,
};
use super::resources::ResourceManager;
use super::resources_res::{IoUsage, MAX_VERTEX_BUFFER_TEXTURES, ResourceReservations};
use crate::accessor::Accessor;
use crate::attr::Stage;
use crate::types::SamplerType;

/// `VertexInfoBufferField`: the index of a field of the `vb_info` buffer (a `vec4` each).
pub mod vertex_info {
    pub const VERTEX_COUNTS: i32 = 0;
    pub const GEOMETRY_COUNTS: i32 = 1;
    pub const VERTEX_STRIDES: i32 = 2;
    pub const VERTEX_OFFSETS: i32 = 3;
}

/// `VertexInfoBuffer.GetStructureType`.
pub fn vertex_info_structure_type() -> StructureType {
    let v4u = AggregateType::VECTOR4.or(AggregateType::U32);
    let arr = AggregateType::ARRAY.or(v4u);
    StructureType {
        fields: vec![
            StructureField::new(v4u, "vertex_counts", 1),
            StructureField::new(v4u, "geometry_counts", 1),
            StructureField::new(arr, "vertex_strides", MAX_VERTEX_BUFFER_TEXTURES),
            StructureField::new(arr, "vertex_offsets", MAX_VERTEX_BUFFER_TEXTURES),
        ],
    }
}

/// The IO usage of the stage in front of this one: `new IoUsage(FeatureFlags.None, 0, -1)`.
pub fn default_vertex_output() -> IoUsage {
    IoUsage {
        used_features: 0,
        clip_distances_written: 0,
        user_defined_map: -1,
    }
}

/// `GetResourceReservations` (no transform feedback emulation: the job accessor never enables it).
pub fn get_resource_reservations(
    tc: &TranslatorContext,
    acc: &mut Accessor,
) -> ResourceReservations {
    let vertex_output = default_vertex_output();
    let mut usage = IoUsage {
        used_features: tc.program.used_features as u32,
        clip_distances_written: tc.program.clip_distances_written,
        user_defined_map: tc.program.attribute_usage.used_output_attributes,
    };
    if tc.definitions.gp_passthrough {
        usage = usage.combine(&vertex_output);
    }
    ResourceReservations::new(acc, false, true, Some(vertex_output), usage)
}

/// `GetDefinitions(vertexAsCompute)`.
pub fn compute_definitions(tc: &TranslatorContext, acc: &Accessor) -> Definitions {
    Definitions::vertex_as_compute(acc.query_geometry_input_fixed(), tc.definitions.gs.clone())
}

pub fn float_array_struct() -> StructureType {
    StructureType {
        fields: vec![StructureField::new(
            AggregateType::ARRAY.or(AggregateType::FP32),
            "data",
            0,
        )],
    }
}

fn uint_array_struct() -> StructureType {
    StructureType {
        fields: vec![StructureField::new(
            AggregateType::ARRAY.or(AggregateType::U32),
            "data",
            0,
        )],
    }
}

/// `CreateResourceManager(vertexAsCompute: true)` of a geometry shader: the reserved resources and the local memories.
pub fn create_geometry_resource_manager(
    tc: &TranslatorContext,
    acc: &mut Accessor,
) -> Result<ResourceManager, String> {
    if tc.stage() != Stage::Geometry {
        return Err("unported: vertex as compute for this stage".into());
    }
    let reservations = get_resource_reservations(tc, acc);
    let mut rm = ResourceManager::new(ShaderStage::Geometry, Some(reservations.clone()));
    let props = &mut rm.properties;
    props.add_or_update_constant_buffer(BufferDefinition {
        layout: BufferLayout::Std140,
        set: 0,
        binding: reservations.vertex_info_constant_buffer_binding,
        name: "vb_info".to_string(),
        ty: vertex_info_structure_type(),
    });
    props.add_or_update_storage_buffer(BufferDefinition {
        layout: BufferLayout::Std430,
        set: 1,
        binding: reservations.vertex_output_storage_buffer_binding,
        name: "vertex_output".to_string(),
        ty: float_array_struct(),
    });
    let remap = reservations.get_topology_remap_buffer_texture_set_and_binding();
    props.add_or_update_texture(TextureDefinition::simple(
        remap.set,
        remap.binding,
        "trb_data",
        SamplerType::TEXTURE_BUFFER,
    ));
    props.add_or_update_storage_buffer(BufferDefinition {
        layout: BufferLayout::Std430,
        set: 1,
        binding: reservations.geometry_vertex_output_storage_buffer_binding,
        name: "geometry_vb_output".to_string(),
        ty: float_array_struct(),
    });
    props.add_or_update_storage_buffer(BufferDefinition {
        layout: BufferLayout::Std430,
        set: 1,
        binding: reservations.geometry_index_output_storage_buffer_binding,
        name: "geometry_ib_output".to_string(),
        ty: uint_array_struct(),
    });
    rm.set_vertex_as_compute_local_memories(
        ShaderStage::Geometry,
        tc.definitions.input_vertex_count(),
    )?;
    set_current_memories(tc, acc, &mut rm);
    Ok(rm)
}
