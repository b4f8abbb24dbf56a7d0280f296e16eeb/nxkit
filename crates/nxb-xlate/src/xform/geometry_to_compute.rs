//! `GeometryToCompute`: a geometry shader that runs as a compute shader reads its input vertices from the vertex
//! output buffer, keeps its output vertex in local memory and writes each emitted vertex and its index to the
//! geometry output buffers.
use super::XformCtx;
use crate::helper::B;
use crate::ir::{Block, Inst, IoVariable, Ir, NodeId, Opd, StorageKind};
use crate::opt::Env;
use crate::translation::definitions::Definitions;
use crate::translation::props::ShaderStage;
use crate::translation::resources_res::ResourceReservations;
use crate::translation::vac::vertex_info::{GEOMETRY_COUNTS, VERTEX_COUNTS};

type R<T> = Result<T, String>;

/// What the generators read: the reservations, the local memory ids and the geometry definitions.
struct Gen<'a> {
    r: &'a ResourceReservations,
    d: &'a Definitions,
    vertex_data: i32,
    topology_remap: i32,
    vertex_count: i32,
    index_count: i32,
}

/// `TryGetOffset`: where the variable lives in the vertex data of an invocation.
fn try_get_offset(
    ir: &Ir,
    r: &ResourceReservations,
    node: NodeId,
    kind: StorageKind,
) -> Option<i32> {
    let op = ir.op(node);
    let is_store = op.inst == Inst::Store;
    let src = |i: usize| op.sources.get(i).copied().flatten().map(|o| ir.value(o));
    let io = IoVariable(src(0)? as u32);
    if io == IoVariable::UserDefined {
        let last = op.sources.len().checked_sub(if is_store { 2 } else { 1 })?;
        r.try_get_offset_user(kind, src(1)?, src(last)?)
    } else if ResourceReservations::is_vector_or_array_variable(io) {
        let last = op.sources.len().checked_sub(if is_store { 2 } else { 1 })?;
        r.try_get_offset_component(kind, io, src(last)?)
    } else {
        r.try_get_offset_var(kind, io)
    }
}

impl Gen<'_> {
    fn increment_local(&self, b: &mut B, id: i32) -> Opd {
        let m = b.ir.const_(id);
        let old = b.op_kind(Inst::Load, StorageKind::LocalMemory, &[m]);
        let one = b.ir.const_(1);
        let new = b.op(Inst::Add, &[old, one]);
        let m = b.ir.const_(id);
        b.raw(Inst::Store, StorageKind::LocalMemory, None, &[m, new]);
        old
    }

    fn load_global_id(&self, b: &mut B, dest: Option<Opd>, component: i32) -> Opd {
        let dest = dest.unwrap_or_else(|| b.ir.local());
        let io = b.ir.const_(IoVariable::GlobalId.0 as i32);
        let c = b.ir.const_(component);
        b.raw(Inst::Load, StorageKind::Input, Some(dest), &[io, c]);
        dest
    }

    fn load_vb_info(&self, b: &mut B, field: i32, component: i32) -> Opd {
        let cb = b.ir.const_(self.r.vertex_info_constant_buffer_binding);
        let f = b.ir.const_(field);
        let c = b.ir.const_(component);
        b.op_kind(Inst::Load, StorageKind::ConstantBuffer, &[cb, f, c])
    }

    /// `GeneratePrimitiveId`.
    fn primitive_id(&self, b: &mut B, dest: Opd, output_index: bool) {
        if self.r.geometry_input_fixed && !output_index {
            // Guest primitive ids are draw-wide while the dispatch is only a slice of the draw.
            let local = self.load_global_id(b, None, 0);
            let base = self.load_vb_info(b, GEOMETRY_COUNTS, 1);
            b.raw(Inst::Add, StorageKind::None, Some(dest), &[local, base]);
            return;
        }
        let field = if self.r.geometry_input_fixed {
            GEOMETRY_COUNTS
        } else {
            VERTEX_COUNTS
        };
        let vertex_count = self.load_vb_info(b, field, 0);
        let vertex_index = self.load_global_id(b, None, 0);
        let instance_index = self.load_global_id(b, None, 1);
        let base = b.op(Inst::Multiply, &[instance_index, vertex_count]);
        b.raw(
            Inst::Add,
            StorageKind::None,
            Some(dest),
            &[base, vertex_index],
        );
    }

    /// `GenerateBaseOffset`.
    fn base_offset(&self, b: &mut B, stride: i32) -> Opd {
        let threads = self.d.threads_per_input_primitive;
        let primitive_id = b.ir.local();
        self.primitive_id(b, primitive_id, true);
        let k = b.ir.const_(stride);
        let base = b.op(Inst::Multiply, &[primitive_id, k]);
        let invocation_id = self.load_global_id(b, None, 2);
        let k = b.ir.const_(stride.checked_div(threads).unwrap_or(0));
        let invocation_offset = b.op(Inst::Multiply, &[invocation_id, k]);
        b.op(Inst::Add, &[base, invocation_offset])
    }

    /// The position of the next output index: `base + count++`.
    fn next_index(&self, b: &mut B) -> Opd {
        let n = self.increment_local(b, self.index_count);
        let stride = self
            .d
            .geometry_output_index_buffer_stride(self.r.geometry_input_fixed);
        let base = self.base_offset(b, stride);
        b.op(Inst::Add, &[base, n])
    }

    fn emit_vertex(&self, b: &mut B) {
        let stride = self.r.output_size_per_invocation;
        let out_vertex = self.increment_local(b, self.vertex_count);
        let base = self.base_offset(
            b,
            self.d.max_output_vertices * self.d.threads_per_input_primitive,
        );
        let output_base_vertex = b.op(Inst::Add, &[base, out_vertex]);
        let output_base_index = self.next_index(b);
        let ib =
            b.ir.const_(self.r.geometry_index_output_storage_buffer_binding);
        let z = b.ir.const_(0);
        b.raw(
            Inst::Store,
            StorageKind::StorageBuffer,
            None,
            &[ib, z, output_base_index, output_base_vertex],
        );
        let k = b.ir.const_(stride);
        let base_offset = b.op(Inst::Multiply, &[output_base_vertex, k]);
        for offset in 0..stride {
            let vertex_offset = if offset > 0 {
                let k = b.ir.const_(offset);
                b.op(Inst::Add, &[base_offset, k])
            } else {
                base_offset
            };
            let m = b.ir.const_(self.vertex_data);
            let k = b.ir.const_(offset);
            let value = b.op_kind(Inst::Load, StorageKind::LocalMemory, &[m, k]);
            let vb =
                b.ir.const_(self.r.geometry_vertex_output_storage_buffer_binding);
            let z = b.ir.const_(0);
            b.raw(
                Inst::Store,
                StorageKind::StorageBuffer,
                None,
                &[vb, z, vertex_offset, value],
            );
        }
    }

    fn end_primitive(&self, b: &mut B) {
        let index = self.next_index(b);
        let ib =
            b.ir.const_(self.r.geometry_index_output_storage_buffer_binding);
        let z = b.ir.const_(0);
        let restart = b.ir.const_(-1);
        b.raw(
            Inst::Store,
            StorageKind::StorageBuffer,
            None,
            &[ib, z, index, restart],
        );
    }

    /// `GenerateVertexOffset`.
    fn vertex_offset(&self, b: &mut B, element_offset: i32, prim_vertex: Opd) -> Opd {
        let vertex_count = self.load_vb_info(b, VERTEX_COUNTS, 0);
        let m = b.ir.const_(self.topology_remap);
        let prim_input_vertex = b.op_kind(Inst::Load, StorageKind::LocalMemory, &[m, prim_vertex]);
        let instance_index = self.load_global_id(b, None, 1);
        let base_vertex = b.op(Inst::Multiply, &[instance_index, vertex_count]);
        let vertex_index = b.op(Inst::Add, &[base_vertex, prim_input_vertex]);
        let k = b.ir.const_(self.r.input_size_per_invocation);
        let base = b.op(Inst::Multiply, &[vertex_index, k]);
        if element_offset != 0 {
            let k = b.ir.const_(element_offset);
            b.op(Inst::Add, &[base, k])
        } else {
            base
        }
    }
}

/// `GeometryToCompute.RunPass` over all blocks.
pub fn run_pass(ir: &mut Ir, blocks: &mut [Block], env: &mut Env, ctx: &mut XformCtx) -> R<()> {
    if ctx.stage != ShaderStage::Geometry {
        return Ok(());
    }
    let Some(d) = ctx.definitions else {
        return Err("GeometryToCompute without definitions".into());
    };
    let Some(r) = env.rm.reservations.clone() else {
        return Err("GeometryToCompute without reservations".into());
    };
    let g = Gen {
        r: &r,
        d,
        vertex_data: env.rm.local_vertex_data_memory_id,
        topology_remap: env.rm.local_topology_remap_memory_id,
        vertex_count: env.rm.local_geometry_output_vertex_count_memory_id,
        index_count: env.rm.local_geometry_output_index_count_memory_id,
    };
    for block in blocks.iter_mut() {
        let mut i = 0;
        while i < block.ops.len() {
            let node = block.ops[i];
            if !ir.is_op(node) {
                i += 1;
                continue;
            }
            let (inst, kind) = (ir.op(node).inst, ir.op(node).storage_kind);
            let dest = ir.op(node).dests.first().copied().flatten();
            let mut b = B::new(ir);
            let mut replaced = true;
            match inst {
                Inst::EmitVertex => g.emit_vertex(&mut b),
                Inst::EndPrimitive => g.end_primitive(&mut b),
                Inst::Load if kind == StorageKind::Input => {
                    replaced = load_input(&g, &mut b, env, node, dest)?;
                }
                Inst::Load if kind == StorageKind::Output => {
                    if let Some(offset) = try_get_offset(b.ir, &r, node, StorageKind::Output) {
                        let m = b.ir.const_(g.vertex_data);
                        let k = b.ir.const_(offset);
                        b.raw(Inst::Load, StorageKind::LocalMemory, dest, &[m, k]);
                    } else {
                        log_invalid(b.ir, env, node, "output");
                        replaced = false;
                    }
                }
                Inst::Store if kind == StorageKind::Output => {
                    if let Some(offset) = try_get_offset(b.ir, &r, node, StorageKind::Output) {
                        let value = b.ir.op(node).sources.last().copied().flatten();
                        let Some(value) = value else {
                            return Err("store without a value".into());
                        };
                        let m = b.ir.const_(g.vertex_data);
                        let k = b.ir.const_(offset);
                        b.raw(Inst::Store, StorageKind::LocalMemory, None, &[m, k, value]);
                    } else {
                        log_invalid(b.ir, env, node, "output");
                        replaced = false;
                    }
                }
                _ => replaced = false,
            }
            let new_nodes = b.take();
            if replaced {
                let n = new_nodes.len();
                for (k, nn) in new_nodes.into_iter().enumerate() {
                    block.ops.insert(i + k, nn);
                }
                crate::opt::utils::delete_node(ir, block, i + n);
                i += n;
            } else {
                i += 1;
            }
        }
    }
    Ok(())
}

fn log_invalid(ir: &Ir, env: &mut Env, node: NodeId, what: &str) {
    let io = ir
        .op(node)
        .sources
        .first()
        .copied()
        .flatten()
        .map(|o| ir.value(o))
        .unwrap_or(-1);
    env.acc.log(&format!(
        "Invalid {what} \"{}\".",
        crate::emit::attrmap::io_name(IoVariable(io as u32))
    ));
}

/// The `Load Input` case. False when the load stays as it is.
fn load_input(g: &Gen, b: &mut B, env: &mut Env, node: NodeId, dest: Option<Opd>) -> R<bool> {
    let io = IoVariable(
        b.ir.op(node)
            .sources
            .first()
            .copied()
            .flatten()
            .map(|o| b.ir.value(o) as u32)
            .ok_or("load without a variable")?,
    );
    if let Some(offset) = try_get_offset(b.ir, g.r, node, StorageKind::Input) {
        let idx = if io == IoVariable::UserDefined { 2 } else { 1 };
        let prim_vertex =
            b.ir.op(node)
                .sources
                .get(idx)
                .copied()
                .flatten()
                .ok_or("input load without a primitive vertex")?;
        let elem = g.vertex_offset(b, offset, prim_vertex);
        let binding = b.ir.const_(g.r.vertex_output_storage_buffer_binding);
        let z = b.ir.const_(0);
        b.raw(
            Inst::Load,
            StorageKind::StorageBuffer,
            dest,
            &[binding, z, elem],
        );
        return Ok(true);
    }
    match io {
        IoVariable::InvocationId => {
            g.load_global_id(b, dest, 2);
        }
        IoVariable::PrimitiveId => {
            let d = dest.ok_or("primitive id load without a destination")?;
            g.primitive_id(b, d, false);
        }
        IoVariable::GlobalId
        | IoVariable::SubgroupEqMask
        | IoVariable::SubgroupGeMask
        | IoVariable::SubgroupGtMask
        | IoVariable::SubgroupLaneId
        | IoVariable::SubgroupLeMask
        | IoVariable::SubgroupLtMask => return Ok(false),
        _ => {
            log_invalid(b.ir, env, node, "input");
            return Ok(false);
        }
    }
    Ok(true)
}
