//! The geometry parts of `EmitterContext`: the vertex-as-compute prologue and return of a geometry shader, the
//! geometry passthrough emulation, and `EmitVertex` / `EndPrimitive`.

use super::emitter::EmitterContext;
use crate::attr::Stage;
use crate::ir::inst::{Inst, IoVariable, StorageKind};
use crate::ir::ops::Opd;
use crate::translation::resources_res::ResourceReservations;
use crate::translation::vac::vertex_info::{GEOMETRY_COUNTS, VERTEX_COUNTS};
use crate::types::SamplerType;

const INT_COORDS: u32 = 1 << 3;

/// `InputTopology` values the passthrough emulation reads.
const LINES_ADJACENCY: i32 = 2;
const TRIANGLES_ADJACENCY: i32 = 4;

impl EmitterContext<'_, '_> {
    fn reservations(&self) -> Result<ResourceReservations, String> {
        self.rm
            .reservations
            .clone()
            .ok_or_else(|| "vertex as compute without reservations".to_string())
    }

    fn global_id(&mut self, component: i32) -> Opd {
        let c = self.ir.const_(component);
        self.load_io_e(StorageKind::Input, IoVariable::GlobalId, None, c)
    }

    fn vb_info(&mut self, binding: i32, field: i32, component: i32) -> Opd {
        let f = self.ir.const_(field);
        let c = self.ir.const_(component);
        self.load_b2(StorageKind::ConstantBuffer, binding, f, c)
    }

    /// The `VertexAsCompute` branch of `EmitStart`.
    pub(super) fn emit_start_vertex_as_compute(&mut self) -> Result<(), String> {
        if self.tc.definitions.stage != Stage::Geometry {
            return Err("unported: vertex as compute for the vertex stage".into());
        }
        let r = self.reservations()?;
        let cb = r.vertex_info_constant_buffer_binding;
        let vertex_offset = self.global_id(0);
        let vertex_count = self.vb_info(cb, GEOMETRY_COUNTS, 0);
        let oob = self.i_compare_greater_or_equal_unsigned(vertex_offset, vertex_count);
        let in_bounds = self.ir.label();
        self.branch_if_false(in_bounds, oob);
        self.return_();
        self.mark_label(in_bounds);

        let instance_offset = self.global_id(1);
        let instance_count = self.vb_info(cb, VERTEX_COUNTS, 1);
        let oob = self.i_compare_greater_or_equal_unsigned(instance_offset, instance_count);
        let in_bounds = self.ir.label();
        self.branch_if_false(in_bounds, oob);
        self.return_();
        self.mark_label(in_bounds);

        let input_vertices = self.tc.definitions.input_vertex_count();
        let k = self.ir.const_(input_vertices);
        let base_vertex = self.i_multiply(vertex_offset, k);
        let remap = self.rm.local_topology_remap_memory_id;
        for index in 0..input_vertices {
            let vertex_index = self.ir.local();
            let ci = self.ir.const_(index);
            let coord = self.i_add(base_vertex, ci);
            self.texture_sample(
                SamplerType::TEXTURE_BUFFER,
                INT_COORDS,
                r.get_topology_remap_buffer_texture_set_and_binding(),
                1,
                &[Some(vertex_index)],
                &[coord],
            );
            let ci = self.ir.const_(index);
            self.store_b2(StorageKind::LocalMemory, remap, ci, vertex_index);
        }
        let zero = self.ir.const_(0);
        let id = self.rm.local_geometry_output_vertex_count_memory_id;
        self.store_b1(StorageKind::LocalMemory, id, zero);
        let zero = self.ir.const_(0);
        let id = self.rm.local_geometry_output_index_count_memory_id;
        self.store_b1(StorageKind::LocalMemory, id, zero);
        if r.geometry_input_fixed {
            let zero = self.ir.const_(0);
            let id = self.rm.local_geometry_primitive_open_memory_id;
            self.store_b1(StorageKind::LocalMemory, id, zero);
            let data = self.rm.local_vertex_data_memory_id;
            for i in 0..r.output_size_per_invocation {
                let ci = self.ir.const_(i);
                let z = self.ir.const_f(0.0);
                self.store_b2(StorageKind::LocalMemory, data, ci, z);
            }
            let default_one = |s: &mut Self, var: IoVariable, location: i32, component: i32| {
                if let Some(offset) =
                    r.try_get_offset(StorageKind::Output, var, location, component)
                {
                    let co = s.ir.const_(offset);
                    let one = s.ir.const_f(1.0);
                    s.store_b2(StorageKind::LocalMemory, data, co, one);
                }
            };
            default_one(self, IoVariable::Position, 0, 3);
            default_one(self, IoVariable::PointSize, 0, 0);
            for i in 0..32 {
                default_one(self, IoVariable::UserDefined, i, 3);
            }
        }
        Ok(())
    }

    /// The `VertexAsCompute` branch of `PrepareForReturn`: pads the index buffer of the invocation with restart indices.
    pub(super) fn vertex_as_compute_return(&mut self) -> Result<(), String> {
        if self.tc.definitions.stage != Stage::Geometry {
            return Err("unported: vertex as compute for the vertex stage".into());
        }
        let r = self.reservations()?;
        let head = self.ir.label();
        let exit = self.ir.label();
        self.mark_label(head);
        let count_id = self.rm.local_geometry_output_index_count_memory_id;
        let written = self.load_b0(StorageKind::LocalMemory, count_id);
        let per_invocation = self
            .tc
            .definitions
            .geometry_output_index_buffer_stride_per_instance(r.geometry_input_fixed);
        let per_primitive = per_invocation * self.tc.definitions.threads_per_input_primitive;
        let limit = self.ir.const_(per_invocation);
        let done = self.i_compare_greater_or_equal_unsigned(written, limit);
        self.branch_if_true(exit, done);

        let cb = r.vertex_info_constant_buffer_binding;
        let primitive_index = self.global_id(0);
        let instance_index = self.global_id(1);
        let invocation_id = self.global_id(2);
        let field = if r.geometry_input_fixed {
            GEOMETRY_COUNTS
        } else {
            VERTEX_COUNTS
        };
        let vertex_count = self.vb_info(cb, field, 0);
        let scaled = self.i_multiply(instance_index, vertex_count);
        let primitive_id = self.i_add(scaled, primitive_index);
        let kp = self.ir.const_(per_primitive);
        let mut ib_offset = self.i_multiply(primitive_id, kp);
        let ki = self.ir.const_(per_invocation);
        let inv_offset = self.i_multiply(invocation_id, ki);
        ib_offset = self.i_add(ib_offset, inv_offset);
        ib_offset = self.i_add(ib_offset, written);
        let c0 = self.ir.const_(0);
        let m1 = self.ir.const_(-1);
        self.store_b3(
            StorageKind::StorageBuffer,
            r.geometry_index_output_storage_buffer_binding,
            c0,
            ib_offset,
            m1,
        );
        let one = self.ir.const_(1);
        let next = self.i_add(written, one);
        self.store_b1(StorageKind::LocalMemory, count_id, next);
        self.branch(head);
        self.mark_label(exit);
        Ok(())
    }

    fn write_position_output(&mut self, prim_index: i32) {
        for c in 0..4 {
            let p = self.ir.const_(prim_index);
            let ci = self.ir.const_(c);
            let v = self.load_io_e(StorageKind::Input, IoVariable::Position, Some(p), ci);
            let co = self.ir.const_(c);
            self.store_io_e(StorageKind::Output, IoVariable::Position, None, co, v);
        }
    }

    fn write_user_defined_output(&mut self, index: i32, prim_index: i32) {
        for c in 0..4 {
            let loc = self.ir.const_(index);
            let p = self.ir.const_(prim_index);
            let ci = self.ir.const_(c);
            let v = self.load_io_a(
                StorageKind::Input,
                IoVariable::UserDefined,
                Some(loc),
                p,
                ci,
            );
            let loc = self.ir.const_(index);
            let co = self.ir.const_(c);
            self.store_io_a(
                StorageKind::Output,
                IoVariable::UserDefined,
                None,
                loc,
                co,
                v,
            );
        }
    }

    /// The geometry branch of `PrepareForReturn`: a passthrough geometry shader is emulated on a host without the feature.
    pub(super) fn geometry_return(&mut self) -> Result<(), String> {
        if !self.tc.definitions.gp_passthrough
            || self.acc.query_host_supports_geometry_shader_passthrough()
        {
            return Ok(());
        }
        let topology = self.tc.definitions.gs.topology;
        let (start, end, step) = match topology {
            LINES_ADJACENCY => (1, 3, 1),
            TRIANGLES_ADJACENCY => (0, 6, 2),
            _ => (0, self.tc.definitions.input_vertex_count_no_adjacency(), 1),
        };
        // A primitive whose viewport mask is 0 goes to no viewport: the hardware drops it.
        let mut culled = None;
        if self.uses_viewport_mask() && crate::translation::host::SUPPORTS_VIEWPORT_MASK {
            let label = self.ir.label();
            culled = Some(label);
            let c0 = self.ir.const_(0);
            let mask = self.load_io_e(StorageKind::Output, IoVariable::ViewportMask, None, c0);
            let z = self.ir.const_(0);
            let is_zero = self.i_compare_equal(mask, z);
            self.branch_if_true(label, is_zero);
        }
        let mut prim = start;
        while prim < end {
            self.write_position_output(prim);
            // Fixed-input stages preserve the complete Maxwell attribute map.
            let mut passthrough = if self.acc.query_geometry_input_fixed() {
                !self.tc.program.attribute_usage.used_output_attributes
            } else {
                0
            };
            while passthrough != 0 {
                let index = passthrough.trailing_zeros() as i32;
                self.write_user_defined_output(index, prim);
                passthrough &= !(1 << index);
            }
            self.emit_vertex()?;
            prim += step;
        }
        self.end_primitive()?;
        if let Some(label) = culled {
            self.mark_label(label);
        }
        Ok(())
    }

    /// `EmitVertex`: a fixed-input geometry shader stops writing at `MaxOutputVertices`.
    pub fn emit_vertex(&mut self) -> Result<(), String> {
        if self.vertex_as_compute && self.reservations()?.geometry_input_fixed {
            let end = self.ir.label();
            let id = self.rm.local_geometry_output_vertex_count_memory_id;
            let count = self.load_b0(StorageKind::LocalMemory, id);
            let max = self.ir.const_(self.tc.definitions.max_output_vertices);
            let full = self.i_compare_greater_or_equal_unsigned(count, max);
            self.branch_if_true(end, full);
            let open = self.rm.local_geometry_primitive_open_memory_id;
            let one = self.ir.const_(1);
            self.store_b1(StorageKind::LocalMemory, open, one);
            self.add_op(Inst::EmitVertex, StorageKind::None, None, &[]);
            self.mark_label(end);
        } else {
            self.add_op(Inst::EmitVertex, StorageKind::None, None, &[]);
        }
        Ok(())
    }

    /// `EndPrimitive`: a fixed-input geometry shader ignores the end of an empty primitive.
    pub fn end_primitive(&mut self) -> Result<(), String> {
        if self.vertex_as_compute && self.reservations()?.geometry_input_fixed {
            let end = self.ir.label();
            let open_id = self.rm.local_geometry_primitive_open_memory_id;
            let open = self.load_b0(StorageKind::LocalMemory, open_id);
            let z = self.ir.const_(0);
            let empty = self.i_compare_equal(open, z);
            self.branch_if_true(end, empty);
            let z = self.ir.const_(0);
            self.store_b1(StorageKind::LocalMemory, open_id, z);
            self.add_op(Inst::EndPrimitive, StorageKind::None, None, &[]);
            self.mark_label(end);
        } else {
            self.add_op(Inst::EndPrimitive, StorageKind::None, None, &[]);
        }
        Ok(())
    }
}
