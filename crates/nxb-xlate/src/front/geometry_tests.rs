//! Synthetic tests of the geometry compute emulation front end.
#![cfg(test)]

use super::emit_test_util::{PRED7, RET};
use super::geometry::{structure_geometry_compute, structure_geometry_reader};
use super::pipeline::options_of;
use crate::accessor::Accessor;
use crate::job::{Job, parse_jobs};
use crate::translation::context::TranslatorContext;

/// `OUT.EMIT_THEN_CUT RZ, RZ, 0` and friends: the type is the two bits at 39.
fn out_i(ty: u64) -> u64 {
    0x3DB8u64 << 50 | 0xff | 0xff << 8 | PRED7 | ty << 39
}

fn gs_job(topology: i32) -> Job {
    let json = format!(r#"[{{"key": "t", "stage": "gs", "geometry_topology": {topology}}}]"#);
    parse_jobs(&json).expect("job").remove(0)
}

/// A Maxwell geometry program: the 0x50 byte header (one thread per primitive, triangle strip, 6 vertices) and the ops.
fn gs_code(ops: &[u64]) -> Vec<u8> {
    let mut h = [0u32; 20];
    h[0] = 4 << 10;
    h[2] = 1 << 24;
    h[3] = 7 << 24;
    h[4] = 6;
    let mut words: Vec<u64> = vec![0];
    for (i, w) in ops.iter().chain([RET].iter()).enumerate() {
        words.push(*w);
        if i % 3 == 2 {
            words.push(0);
        }
    }
    let mut code: Vec<u8> = h.iter().flat_map(|w| w.to_le_bytes()).collect();
    code.extend(words.iter().flat_map(|w| w.to_le_bytes()));
    code
}

fn context<'a>(job: &'a Job, ops: &[u64]) -> (TranslatorContext, Accessor<'a>) {
    let mut acc = Accessor::new(job);
    let tc = TranslatorContext::create(&gs_code(ops), &mut acc, options_of(job)).expect("context");
    (tc, acc)
}

#[test]
fn geometry_compute_structures_with_a_fixed_input() {
    let job = gs_job(3);
    let (tc, mut acc) = context(&job, &[out_i(3)]);
    let (info, props, program) =
        structure_geometry_compute(&tc, &mut acc, true).expect("structure");
    assert_eq!(props.definitions.compute_local_size, [32, 1, 1]);
    assert!(program.geometry_stride > 0);
    assert!(!info.functions.is_empty());
    let names: Vec<&str> = props
        .properties
        .storage_buffers
        .iter()
        .map(|b| b.name.as_str())
        .collect();
    assert!(names.contains(&"vertex_output"));
    assert!(names.contains(&"geometry_vb_output"));
    assert!(names.contains(&"geometry_ib_output"));
    let cbs: Vec<&str> = props
        .properties
        .constant_buffers
        .iter()
        .map(|b| b.name.as_str())
        .collect();
    assert!(cbs.contains(&"vb_info"));
}

#[test]
fn geometry_compute_removes_the_emit_instructions() {
    let job = gs_job(3);
    let (tc, mut acc) = context(&job, &[out_i(3)]);
    let mut t = super::pipeline::run_to_xform_mode(&tc, &mut acc, true).expect("xform");
    let f = t.funcs.remove(0);
    let any = f.blocks.iter().flat_map(|b| b.ops.iter()).any(|&n| {
        matches!(
            &t.ir.nodes[n.0 as usize],
            crate::ir::Node::Op(o) if matches!(o.inst, crate::ir::Inst::EmitVertex | crate::ir::Inst::EndPrimitive)
        )
    });
    assert!(!any);
}

#[test]
fn geometry_reader_copies_the_reserved_outputs() {
    let job = gs_job(3);
    let (tc, mut acc) = context(&job, &[out_i(3)]);
    let (info, props) = structure_geometry_reader(&tc, &mut acc, true).expect("reader");
    assert!(!info.functions.is_empty());
    assert_eq!(props.definitions.stage, Some(crate::attr::Stage::Vertex));
    assert_eq!(props.properties.storage_buffers[0].name, "vb_input");
}

#[test]
fn geometry_compute_writes_the_index_buffer() {
    let job = gs_job(3);
    let (tc, mut acc) = context(&job, &[out_i(3)]);
    let t = super::pipeline::run_to_xform_mode(&tc, &mut acc, true).expect("xform");
    let stores = t.funcs[0]
        .blocks
        .iter()
        .flat_map(|b| b.ops.iter())
        .filter(|&&n| {
            matches!(
                &t.ir.nodes[n.0 as usize],
                crate::ir::Node::Op(o) if o.inst == crate::ir::Inst::Store
                    && o.storage_kind == crate::ir::StorageKind::StorageBuffer
            )
        })
        .count();
    // the emitted vertex, its index and the primitive restart marker
    assert!(stores >= 3, "{stores} stores");
}

#[test]
fn input_vertex_counts_follow_the_topology() {
    use crate::translation::definitions::{Definitions, topo};
    let job = gs_job(3);
    let (tc, _) = context(&job, &[out_i(3)]);
    let mut d: Definitions = tc.definitions.clone();
    let counts = [
        (topo::POINTS, 1, 1),
        (topo::LINES, 2, 2),
        (topo::LINES_ADJACENCY, 4, 2),
        (topo::TRIANGLES, 3, 3),
        (topo::TRIANGLES_ADJACENCY, 6, 3),
    ];
    for (t, with, without) in counts {
        d.gs.topology = t;
        assert_eq!(d.input_vertex_count(), with);
        assert_eq!(d.input_vertex_count_no_adjacency(), without);
    }
}

#[test]
fn index_buffer_stride_depends_on_the_input() {
    let job = gs_job(3);
    let (tc, _) = context(&job, &[out_i(3)]);
    let d = &tc.definitions;
    assert_eq!(d.geometry_output_index_buffer_stride_per_instance(true), 12);
    // 6 vertices plus 6 / 3 primitives of a triangle strip
    assert_eq!(d.geometry_output_index_buffer_stride_per_instance(false), 8);
}

#[test]
fn only_a_geometry_shader_runs_as_compute() {
    let job = parse_jobs(r#"[{"key": "t", "stage": "vs"}]"#)
        .expect("job")
        .remove(0);
    let mut h = [0u32; 20];
    h[0] = 1 << 10;
    let mut code: Vec<u8> = h.iter().flat_map(|w| w.to_le_bytes()).collect();
    code.extend([0u64, RET, 0, 0].iter().flat_map(|w| w.to_le_bytes()));
    let mut acc = Accessor::new(&job);
    let tc = TranslatorContext::create(&code, &mut acc, options_of(&job)).expect("context");
    let e = structure_geometry_compute(&tc, &mut acc, true).expect_err("vertex");
    assert_eq!(
        e,
        "unported: vertex as compute of a vertex shader, only geometry shaders run as compute"
    );
}
