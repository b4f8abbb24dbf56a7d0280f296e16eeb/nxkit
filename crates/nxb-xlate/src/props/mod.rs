//! Everything the back end reads besides the structured program: the shader definitions, host capabilities, resource
//! properties and attribute usage as plain data. The probe phase `props` dumps it (see `tools/xlate-probe/Probe.cs`,
//! `Props`) and [`Props::parse`] reads that text; the translation core fills the same structs from the real objects.
//! These are the back end's view. The front end's port of `ShaderProperties` is `translation::props`.

pub mod text;

use crate::attr::Stage;
use crate::ir::IoVariable;

/// `AggregateType` (a number, as the C# flags enum).
pub type Agg = crate::ir::agg::AggType;

/// `ShaderDefinitions` as far as the back end reads it.
#[derive(Clone, Debug, Default)]
pub struct Definitions {
    pub stage: Option<Stage>,
    pub compute_local_size: [i32; 3],
    pub gp_passthrough: bool,
    pub last_in_vertex_pipeline: bool,
    pub threads_per_input_primitive: i32,
    pub input_topology: i32,
    pub output_topology: i32,
    pub max_output_vertices: i32,
    pub dual_source_blend: bool,
    pub early_z_force: bool,
    pub y_negate_enabled: bool,
    pub origin_upper_left: bool,
    pub tess_cw: bool,
    pub tess_patch_type: i32,
    pub tess_spacing: i32,
    pub ia_indexing: bool,
    pub oa_indexing: bool,
    pub transform_feedback_enabled: bool,
    pub supports_scaled_vertex_formats: bool,
    /// `ImapTypes[i]`: x, y, z, w (`PixelImap`)
    pub imap_types: Vec<[u8; 4]>,
    /// `AttributeTypes[i]` (`AttributeType`)
    pub attribute_types: Vec<u32>,
    /// `GetFragmentOutputColorType(i)` (Vector4 | element type)
    pub fragment_output_types: Vec<Agg>,
}

impl Definitions {
    pub fn stage(&self) -> Result<Stage, String> {
        self.stage.ok_or_else(|| "no shader stage".to_string())
    }

    /// `HasTransformFeedbackOutputs(isOutput)`.
    pub fn has_transform_feedback_outputs(&self, is_output: bool) -> bool {
        self.transform_feedback_enabled
            && ((is_output && self.last_in_vertex_pipeline)
                || (!is_output && self.stage == Some(Stage::Fragment)))
    }

    /// `HasPerLocationInputOrOutput`.
    pub fn has_per_location_input_or_output(&self, v: IoVariable, is_output: bool) -> bool {
        if v == IoVariable::UserDefined {
            return (!is_output && !self.ia_indexing) || (is_output && !self.oa_indexing);
        }
        v == IoVariable::FragmentOutputColor
    }

    /// `HasPerLocationInputOrOutputComponent`: only with transform feedback in the definitions. The job accessor never
    /// enables it. A capture is the entry `main0_tf` of the MSL back end, which reads the finished outputs.
    pub fn has_per_location_input_or_output_component(
        &self,
        v: IoVariable,
        _location: i32,
        _component: i32,
        is_output: bool,
    ) -> bool {
        // `msl::library::build` refuses definitions with transform feedback before this can matter
        let _ = (v, is_output);
        false
    }

    /// `GetUserDefinedType`.
    pub fn user_defined_type(&self, location: i32, is_output: bool) -> Result<Agg, String> {
        if (!is_output && self.ia_indexing) || (is_output && self.oa_indexing) {
            return Ok(Agg::ARRAY | Agg::VECTOR4 | Agg::FP32);
        }
        let mut ty = Agg::VECTOR4;
        if self.stage == Some(Stage::Vertex) && !is_output {
            let at = *self
                .attribute_types
                .get(location as usize)
                .ok_or("attribute type missing")?;
            ty |= attribute_as_aggregate(at, self.supports_scaled_vertex_formats)?;
        } else {
            ty |= Agg::FP32;
        }
        Ok(ty)
    }

    /// `GetFragmentOutputColorType`.
    pub fn fragment_output_color_type(&self, location: i32) -> Result<Agg, String> {
        self.fragment_output_types
            .get(location as usize)
            .copied()
            .ok_or_else(|| "fragment output type missing".to_string())
    }
}

/// `AttributeType.AsAggregate`.
pub fn attribute_as_aggregate(at: u32, scaled: bool) -> Result<Agg, String> {
    Ok(match at & !0xc0 {
        0 => Agg::FP32,
        1 => Agg::S32,
        2 => Agg::U32,
        3 => {
            if scaled {
                Agg::FP32
            } else {
                Agg::S32
            }
        }
        4 => {
            if scaled {
                Agg::FP32
            } else {
                Agg::U32
            }
        }
        _ => return Err(format!("invalid attribute type {at}")),
    })
}

/// `HostCapabilities`.
#[derive(Clone, Debug, Default)]
pub struct HostCaps {
    pub reduced_precision: bool,
    pub supports_fragment_shader_interlock: bool,
    pub supports_fragment_shader_ordering_intel: bool,
    pub supports_geometry_shader_passthrough: bool,
    pub supports_shader_ballot: bool,
    pub supports_shader_barrier_divergence: bool,
    pub supports_shader_float64: bool,
    pub supports_shader_non_uniform_indexing: bool,
    pub supports_texture_shadow_lod: bool,
    pub supports_viewport_mask: bool,
    pub metal_image_atomics: bool,
}

/// `StructureField`.
#[derive(Clone, Debug)]
pub struct Field {
    pub ty: Agg,
    pub array_length: i32,
    pub name: String,
}

/// `BufferDefinition` (layout 0 is Std140, 1 Std430).
#[derive(Clone, Debug)]
pub struct BufferDef {
    pub layout: i32,
    pub set: i32,
    pub binding: i32,
    pub name: String,
    pub fields: Vec<Field>,
}

/// `TextureDefinition`.
#[derive(Clone, Debug)]
pub struct TextureDef {
    pub set: i32,
    pub binding: i32,
    pub array_length: i32,
    pub separate: bool,
    pub name: String,
    pub ty: crate::types::SamplerType,
    pub format: usize,
    pub flags: i32,
}

/// `MemoryDefinition` with its id.
#[derive(Clone, Debug)]
pub struct MemoryDef {
    pub id: i32,
    pub name: String,
    pub ty: Agg,
    pub array_length: i32,
    /// The memory starts at zero in each invocation (`MemoryDefinition.ZeroInit`).
    pub zero_init: bool,
}

/// `ShaderProperties` (every list in dictionary insertion order).
#[derive(Clone, Debug, Default)]
pub struct Properties {
    pub constant_buffers: Vec<BufferDef>,
    pub storage_buffers: Vec<BufferDef>,
    pub textures: Vec<TextureDef>,
    pub images: Vec<TextureDef>,
    pub local_memories: Vec<MemoryDef>,
    pub shared_memories: Vec<MemoryDef>,
    pub uses_physical_pointers: bool,
}

/// Everything besides the structured program.
#[derive(Clone, Debug, Default)]
pub struct Props {
    pub definitions: Definitions,
    pub host: HostCaps,
    pub properties: Properties,
    pub passthrough_attributes: i32,
    /// `TargetApi` (0 OpenGL, 1 Vulkan).
    pub target_api: i32,
    /// constant buffer slot to (set, binding) of the resource manager after the structuring
    pub cb_slot_to_binding: Vec<(i32, i32, i32)>,
    pub helper_functions_mask: i32,
    /// `Info.IoDefinitions` in insertion order: (storage kind, io variable, location, component)
    pub io_definitions: Vec<(u32, u32, i32, i32)>,
}
