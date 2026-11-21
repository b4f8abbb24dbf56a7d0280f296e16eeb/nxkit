//! Part of the translator port (see docs/translator-port.md).
//! `ResourceManager` (Translation/ResourceManager.cs): bindings, the resource definitions and the descriptors for the driver.
//! The accessor is passed to each method that queries it.

use super::props::*;
use super::resources_res::ResourceReservations;
use crate::accessor::Accessor;
use crate::types::{SamplerType, unpack_offsets};
use std::collections::BTreeSet;

pub(super) const DEFAULT_LOCAL_MEMORY_SIZE: i32 = 128;
pub(super) const DEFAULT_SHARED_MEMORY_SIZE: i32 = 4096;
/// `Constants.ConstantBufferSize` in bytes.
pub const CONSTANT_BUFFER_SIZE: i32 = 0x10000;
pub(super) const STAGE_PREFIXES: [&str; 6] = ["cp", "vp", "tcp", "tep", "gp", "fp"];
pub(super) const NO_BINDING: SetBindingPair = SetBindingPair {
    set: -1,
    binding: -1,
};

/// `TextureInfo`: the key of a used texture or image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureInfo {
    pub cbuf_slot: i32,
    pub handle: i32,
    pub array_length: i32,
    pub separate: bool,
    pub ty: SamplerType,
    pub format: usize,
}

/// `TextureMeta`.
#[derive(Clone, Copy, Debug)]
pub struct TextureMeta {
    pub set: i32,
    pub binding: i32,
    pub accurate_type: bool,
    pub ty: SamplerType,
    pub usage_flags: TextureUsageFlags,
}

/// `ResourceManager`.
pub struct ResourceManager {
    pub(super) stage: ShaderStage,
    pub(super) stage_prefix: &'static str,
    pub(super) cb_slot_to_binding: [SetBindingPair; 25],
    pub(super) sb_slot_to_binding: [SetBindingPair; 32],
    pub(super) sb_slot_written: u32,
    /// packed (sbCbSlot, sbCbOffset) to slot, in insertion order
    pub(super) sb_slots: OrdMap<i32, i32>,
    pub(super) used_constant_buffer_bindings: BTreeSet<i32>,
    pub(super) used_textures: OrdMap<TextureInfo, TextureMeta>,
    pub(super) used_images: OrdMap<TextureInfo, TextureMeta>,
    pub local_memory_id: i32,
    pub shared_memory_id: i32,
    pub local_vertex_data_memory_id: i32,
    pub local_topology_remap_memory_id: i32,
    pub local_vertex_index_vertex_rate_memory_id: i32,
    pub local_vertex_index_instance_rate_memory_id: i32,
    pub local_geometry_output_vertex_count_memory_id: i32,
    pub local_geometry_output_index_count_memory_id: i32,
    pub local_geometry_primitive_open_memory_id: i32,
    pub hmma_scratch_memory_id: i32,
    pub properties: ShaderProperties,
    pub reservations: Option<ResourceReservations>,
}

/// `GetShaderStagePrefix`.
pub fn get_shader_stage_prefix(stage: ShaderStage) -> &'static str {
    STAGE_PREFIXES
        .get(stage as usize)
        .copied()
        .unwrap_or("invalid")
}

pub(super) fn pack_sb_cb_info(sb_cb_slot: i32, sb_cb_offset: i32) -> i32 {
    sb_cb_offset | (sb_cb_slot << 16)
}

pub(super) fn unpack_sb_cb_info(key: i32) -> (i32, i32) {
    (((key >> 16) as u8) as i32, (key as u16) as i32)
}

pub(super) fn merge_texture_meta(mut meta: TextureMeta, existing: TextureMeta) -> TextureMeta {
    meta.set = existing.set;
    meta.binding = existing.binding;
    meta.usage_flags |= existing.usage_flags;
    // an inaccurate type gives way to an accurate one
    if existing.accurate_type {
        meta.accurate_type = true;
        meta.ty = existing.ty;
    }
    meta
}

impl ResourceManager {
    /// `new ResourceManager(stage, gpuAccessor, reservations)`.
    pub fn new(stage: ShaderStage, reservations: Option<ResourceReservations>) -> Self {
        let mut properties = ShaderProperties::new();
        properties.add_or_update_constant_buffer(BufferDefinition {
            layout: BufferLayout::Std140,
            set: 0,
            binding: support_buffer::BINDING,
            name: "support_buffer".to_string(),
            ty: support_buffer::structure_type(),
        });
        ResourceManager {
            stage,
            stage_prefix: get_shader_stage_prefix(stage),
            cb_slot_to_binding: [NO_BINDING; 25],
            sb_slot_to_binding: [NO_BINDING; 32],
            sb_slot_written: 0,
            sb_slots: OrdMap::new(),
            used_constant_buffer_bindings: BTreeSet::new(),
            used_textures: OrdMap::new(),
            used_images: OrdMap::new(),
            local_memory_id: -1,
            shared_memory_id: -1,
            local_vertex_data_memory_id: 0,
            local_topology_remap_memory_id: 0,
            local_vertex_index_vertex_rate_memory_id: 0,
            local_vertex_index_instance_rate_memory_id: 0,
            local_geometry_output_vertex_count_memory_id: 0,
            local_geometry_output_index_count_memory_id: 0,
            local_geometry_primitive_open_memory_id: 0,
            hmma_scratch_memory_id: -1,
            properties,
            reservations,
        }
    }

    pub fn stage(&self) -> ShaderStage {
        self.stage
    }

    pub fn set_current_local_memory(&mut self, size: i32, is_used: bool) {
        if is_used {
            let size = if size <= 0 {
                DEFAULT_LOCAL_MEMORY_SIZE
            } else {
                size
            };
            let lmem = MemoryDefinition::new(
                "local_memory",
                AggregateType::ARRAY.or(AggregateType::U32),
                (size + 3) / 4,
            );
            self.local_memory_id = self.properties.add_local_memory(lmem);
        } else {
            self.local_memory_id = -1;
        }
    }

    pub fn set_current_shared_memory(&mut self, size: i32, is_used: bool) {
        if is_used {
            let size = if size <= 0 {
                DEFAULT_SHARED_MEMORY_SIZE
            } else {
                size
            };
            let smem = MemoryDefinition::new(
                "shared_memory",
                AggregateType::ARRAY.or(AggregateType::U32),
                (size + 3) / 4,
            );
            self.shared_memory_id = self.properties.add_shared_memory(smem);
        } else {
            self.shared_memory_id = -1;
        }
    }

    /// `AddRawLastEntryMemory`: the pool table entry a raw global memory helper chose last in this invocation, one
    /// array of `words` words for each helper (the entry's index and a copy of its range and delta, see
    /// `opt/gts_raw.rs`), zero at the start.
    pub fn add_raw_last_entry_memory(&mut self, words: i32) -> i32 {
        let name = format!("gmem_last_{}", self.properties.local_memories.len());
        let mut m =
            MemoryDefinition::new(&name, AggregateType::ARRAY.or(AggregateType::U32), words);
        m.zero_init = true;
        self.properties.add_local_memory(m)
    }

    /// `GetHmmaScratchMemoryId`.
    pub fn get_hmma_scratch_memory_id(&mut self, subgroups: i32) -> i32 {
        if self.hmma_scratch_memory_id < 0 {
            self.hmma_scratch_memory_id = self.properties.add_shared_memory(MemoryDefinition::new(
                "hmma_scratch",
                AggregateType::ARRAY.or(AggregateType::U32),
                256 * subgroups,
            ));
        }
        self.hmma_scratch_memory_id
    }

    /// `SetVertexAsComputeLocalMemories`.`input_vertex_count` is `inputTopology.InputVertexCount`.
    pub fn set_vertex_as_compute_local_memories(
        &mut self,
        stage: ShaderStage,
        input_vertex_count: i32,
    ) -> Result<(), String> {
        let (out_size, fixed) = match &self.reservations {
            Some(r) => (r.output_size_per_invocation, r.geometry_input_fixed),
            None => return Err("vertex as compute without reservations".into()),
        };
        self.local_vertex_data_memory_id = self.add_memory_definition(
            "local_vertex_data",
            AggregateType::ARRAY.or(AggregateType::FP32),
            out_size,
        );
        if stage == ShaderStage::Vertex {
            self.local_vertex_index_vertex_rate_memory_id =
                self.add_memory_definition("local_vertex_index_vr", AggregateType::U32, 1);
            self.local_vertex_index_instance_rate_memory_id =
                self.add_memory_definition("local_vertex_index_ir", AggregateType::U32, 1);
        } else if stage == ShaderStage::Geometry {
            self.local_topology_remap_memory_id = self.add_memory_definition(
                "local_topology_remap",
                AggregateType::ARRAY.or(AggregateType::U32),
                input_vertex_count,
            );
            self.local_geometry_output_vertex_count_memory_id =
                self.add_memory_definition("local_geometry_output_vertex", AggregateType::U32, 1);
            self.local_geometry_output_index_count_memory_id =
                self.add_memory_definition("local_geometry_output_index", AggregateType::U32, 1);
            if fixed {
                self.local_geometry_primitive_open_memory_id = self.add_memory_definition(
                    "local_geometry_primitive_open",
                    AggregateType::U32,
                    1,
                );
            }
        }
        Ok(())
    }

    fn add_memory_definition(&mut self, name: &str, ty: AggregateType, array_length: i32) -> i32 {
        self.properties
            .add_local_memory(MemoryDefinition::new(name, ty, array_length))
    }

    pub fn get_constant_buffer_binding(
        &mut self,
        acc: &mut Accessor,
        slot: i32,
    ) -> Result<i32, String> {
        let idx = usize::try_from(slot)
            .ok()
            .filter(|&i| i < self.cb_slot_to_binding.len())
            .ok_or_else(|| format!("constant buffer slot {slot} out of range"))?;
        let mut sb = self.cb_slot_to_binding[idx];
        if sb.binding < 0 {
            sb = acc.create_constant_buffer_binding(slot);
            self.cb_slot_to_binding[idx] = sb;
            let name = format!("{}_c{}", self.stage_prefix, slot);
            self.add_new_constant_buffer(sb.set, sb.binding, name);
        }
        Ok(sb.binding)
    }

    /// `TryGetStorageBufferBinding`: `None` when the slots are exhausted.
    pub fn try_get_storage_buffer_binding(
        &mut self,
        acc: &mut Accessor,
        sb_cb_slot: i32,
        sb_cb_offset: i32,
        write: bool,
    ) -> Option<i32> {
        let slot = self.try_get_sb_slot((sb_cb_slot as u8) as i32, (sb_cb_offset as u16) as i32)?;
        let mut sb = self.sb_slot_to_binding[slot as usize];
        if sb.binding < 0 {
            sb = acc.create_storage_buffer_binding(slot);
            self.sb_slot_to_binding[slot as usize] = sb;
            let name = format!("{}_s{}", self.stage_prefix, slot);
            self.add_new_storage_buffer(sb.set, sb.binding, name);
        }
        if write {
            self.sb_slot_written |= 1u32 << slot;
        }
        Some(sb.binding)
    }

    fn try_get_sb_slot(&mut self, sb_cb_slot: i32, sb_cb_offset: i32) -> Option<i32> {
        let key = pack_sb_cb_info(sb_cb_slot, sb_cb_offset);
        if let Some(&s) = self.sb_slots.get(&key) {
            return Some(s);
        }
        let slot = self.sb_slots.len() as i32;
        if slot as usize >= self.sb_slot_to_binding.len() {
            return None;
        }
        self.sb_slots.insert(key, slot);
        Some(slot)
    }

    /// `TryGetConstantBufferSlot`.
    pub fn try_get_constant_buffer_slot(&self, binding: i32) -> Option<i32> {
        self.cb_slot_to_binding
            .iter()
            .position(|b| b.binding == binding)
            .map(|s| s as i32)
    }

    /// `GetTextureTypes`: the accurate types of the textures (or images) whose handle word is at `texture_offset`
    /// of constant buffer `cbuf_slot`, arrays are left out.
    pub fn get_texture_types(
        &self,
        cbuf_slot: i32,
        texture_offset: i32,
        is_image: bool,
    ) -> Vec<SamplerType> {
        let dict = if is_image {
            &self.used_images
        } else {
            &self.used_textures
        };
        let mut types = Vec::new();
        for (info, meta) in dict.iter() {
            let (offset, _, _) = unpack_offsets(info.handle);
            let slot = unpack_slots(info.cbuf_slot, cbuf_slot).0;
            if info.array_length == 1
                && meta.accurate_type
                && slot == cbuf_slot
                && offset == texture_offset
            {
                types.push(meta.ty);
            }
        }
        types
    }

    /// `SetUsageFlagsForTextureQuery`.
    pub fn set_usage_flags_for_texture_query(
        &mut self,
        binding: i32,
        ty: SamplerType,
    ) -> Result<(), String> {
        let found = self
            .used_textures
            .iter()
            .find(|(_, m)| m.binding == binding)
            .map(|(i, m)| (*i, *m));
        if let Some((info, mut meta)) = found {
            meta.usage_flags |= TextureUsageFlags::NEEDS_SCALE_VALUE;
            let dims = dimensions(ty)?;
            let can_scale =
                self.stage.supports_render_scale() && info.array_length == 1 && dims == 2;
            if !can_scale {
                meta.usage_flags |= TextureUsageFlags::RES_SCALE_UNSUPPORTED;
            }
            self.used_textures.insert(info, meta);
        }
        Ok(())
    }

    pub fn set_used_constant_buffer_binding(&mut self, binding: i32) {
        self.used_constant_buffer_bindings.insert(binding);
    }

    pub fn get_constant_buffer_descriptors(&self) -> Vec<BufferDescriptor> {
        let mut d = Vec::new();
        for (slot, sb) in self.cb_slot_to_binding.iter().enumerate() {
            if sb.binding >= 0 && self.used_constant_buffer_bindings.contains(&sb.binding) {
                d.push(BufferDescriptor::cbuf(sb.set, sb.binding, slot as i32));
            }
        }
        d
    }

    pub fn get_storage_buffer_descriptors(&self) -> Vec<BufferDescriptor> {
        let mut d = Vec::new();
        for (key, slot) in self.sb_slots.iter() {
            let sb = self.sb_slot_to_binding[*slot as usize];
            if sb.binding >= 0 {
                let (cb_slot, cb_offset) = unpack_sb_cb_info(*key);
                let flags = if self.sb_slot_written & (1u32 << slot) != 0 {
                    BufferUsageFlags::WRITE
                } else {
                    BufferUsageFlags::NONE
                };
                d.push(BufferDescriptor::sbuf(
                    sb.set, sb.binding, *slot, cb_slot, cb_offset, flags,
                ));
            }
        }
        d
    }

    pub fn get_texture_descriptors(&self, include_arrays: bool) -> Vec<TextureDescriptor> {
        get_descriptors(&self.used_textures, include_arrays)
    }

    pub fn get_image_descriptors(&self, include_arrays: bool) -> Vec<TextureDescriptor> {
        get_descriptors(&self.used_images, include_arrays)
    }

    /// `TryGetCbufSlotAndHandleForTexture`: `(cbufSlot, handle)`.
    pub fn try_get_cbuf_slot_and_handle_for_texture(&self, binding: i32) -> Option<(i32, i32)> {
        self.used_textures
            .iter()
            .find(|(_, m)| m.binding == binding)
            .map(|(i, _)| (i.cbuf_slot, i.handle))
    }

    /// `FindTextureDescriptorIndex`: -1 when absent.
    pub fn find_texture_descriptor_index(&self, binding: i32) -> i32 {
        find_descriptor_index(&self.get_texture_descriptors(true), binding)
    }

    /// `FindImageDescriptorIndex`: -1 when absent.
    pub fn find_image_descriptor_index(&self, binding: i32) -> i32 {
        find_descriptor_index(&self.get_image_descriptors(true), binding)
    }

    pub fn is_array_of_textures_or_images(&self, binding: i32, is_image: bool) -> bool {
        let dict = if is_image {
            &self.used_images
        } else {
            &self.used_textures
        };
        dict.iter()
            .find(|(_, m)| m.binding == binding)
            .is_some_and(|(i, _)| i.array_length != 1)
    }

    fn add_new_constant_buffer(&mut self, set: i32, binding: i32, name: String) {
        let ty = StructureType {
            fields: vec![StructureField::new(
                AggregateType::ARRAY
                    .or(AggregateType::VECTOR4)
                    .or(AggregateType::FP32),
                "data",
                CONSTANT_BUFFER_SIZE / 16,
            )],
        };
        self.properties
            .add_or_update_constant_buffer(BufferDefinition {
                layout: BufferLayout::Std140,
                set,
                binding,
                name,
                ty,
            });
    }

    fn add_new_storage_buffer(&mut self, set: i32, binding: i32, name: String) {
        let ty = StructureType {
            fields: vec![StructureField::new(
                AggregateType::ARRAY.or(AggregateType::U32),
                "data",
                0,
            )],
        };
        self.properties
            .add_or_update_storage_buffer(BufferDefinition {
                layout: BufferLayout::Std430,
                set,
                binding,
                name,
                ty,
            });
    }
}

/// `TextureHandle.UnpackSlots`: `(textureBufferIndex, samplerBufferIndex)`.
pub fn unpack_slots(slots: i32, default_texture_buffer_index: i32) -> (i32, i32) {
    if slots < 0 {
        (default_texture_buffer_index, default_texture_buffer_index)
    } else {
        let high = (slots as u32) >> 16;
        let tex = (slots as u16) as i32;
        (tex, if high != 0 { high as i32 - 1 } else { tex })
    }
}

pub(super) fn find_descriptor_index(a: &[TextureDescriptor], binding: i32) -> i32 {
    a.iter()
        .position(|d| d.binding == binding)
        .map_or(-1, |i| i as i32)
}

pub(super) fn descriptor(info: &TextureInfo, meta: &TextureMeta) -> TextureDescriptor {
    TextureDescriptor {
        set: meta.set,
        binding: meta.binding,
        ty: meta.ty,
        format: info.format,
        cbuf_slot: info.cbuf_slot,
        handle_index: info.handle,
        array_length: info.array_length,
        separate: info.separate,
        flags: meta.usage_flags,
    }
}

/// `GetDescriptors`: the non-arrays in insertion order, then the arrays.
pub(super) fn get_descriptors(
    used: &OrdMap<TextureInfo, TextureMeta>,
    include_arrays: bool,
) -> Vec<TextureDescriptor> {
    let mut d = Vec::new();
    let mut has_any_array = false;
    for (info, meta) in used.iter() {
        if info.array_length > 1 {
            has_any_array = true;
            continue;
        }
        d.push(descriptor(info, meta));
    }
    if has_any_array && include_arrays {
        for (info, meta) in used.iter() {
            if info.array_length > 1 {
                d.push(descriptor(info, meta));
            }
        }
    }
    d
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
