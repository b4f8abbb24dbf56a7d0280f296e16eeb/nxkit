//! Tests of indexed attributes, the viewport mask and the draw index.

use super::gaps_tests::{msl, translate};
use super::tests::{B, F4};
use crate::attr::Stage;
use crate::ir::agg::AggType;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::structured::ast::A;

fn notes(b: &B) -> Vec<String> {
    translate(b).libraries[0]
        .reflection
        .clone()
        .expect("reflection")
        .notes
}

/// A pixel shader that reads element `elem` of attribute `index`, which is a constant or the second input.
pub(super) fn indexed_pixel(index: Option<i32>, elem: i32) -> B {
    let mut b = B::new(Stage::Fragment, "ps");
    let d = &mut b.s.props.definitions;
    d.ia_indexing = true;
    d.imap_types = vec![[2, 2, 2, 2], [0; 4], [1, 1, 1, 1]];
    d.fragment_output_types = vec![F4];
    b.io(StorageKind::Input, IoVariable::UserDefined, 0);
    b.io(StorageKind::Output, IoVariable::FragmentOutputColor, 0);
    let i = match index {
        Some(c) => b.k(c),
        None => {
            b.io(StorageKind::Input, IoVariable::SampleId, 0);
            b.load_scalar(IoVariable::SampleId, AggType::U32)
        }
    };
    let src = vec![b.k(IoVariable::UserDefined.0 as i32), i, b.k(elem)];
    let op = b.op(Inst::Load, StorageKind::Input, 0, src);
    let v = b.def(AggType::FP32, op);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, v);
    b
}

/// A vertex shader that stores into element `elem` of output attribute `index`.
pub(super) fn indexed_vertex(index: Option<i32>, elem: i32) -> B {
    let mut b = B::new(Stage::Vertex, "vs");
    let d = &mut b.s.props.definitions;
    d.oa_indexing = true;
    d.ia_indexing = true;
    d.attribute_types = vec![0, 1, 0];
    b.io(StorageKind::Input, IoVariable::UserDefined, 0);
    b.io(StorageKind::Output, IoVariable::UserDefined, 0);
    b.io(StorageKind::Output, IoVariable::Position, 0);
    let i = match index {
        Some(c) => b.k(c),
        None => {
            b.io(StorageKind::Input, IoVariable::VertexIndex, 0);
            b.load_scalar(IoVariable::VertexIndex, AggType::U32)
        }
    };
    let src = vec![b.k(IoVariable::UserDefined.0 as i32), i, b.k(elem)];
    let op = b.op(Inst::Load, StorageKind::Input, 0, src);
    let v = b.def(AggType::FP32, op);
    let src = vec![b.k(IoVariable::UserDefined.0 as i32), i, b.k(elem), v];
    let st = b.op(Inst::Store, StorageKind::Output, 0, src);
    b.put(st);
    b.store_io(IoVariable::Position, None, 0, v);
    let r = b.op(Inst::Return, StorageKind::None, 0, vec![]);
    b.put(r);
    b
}

#[test]
fn a_register_index_picks_the_member_of_a_pixel_input() {
    let m = msl(&indexed_pixel(None, 2));
    for want in [
        "float4 loc0 [[user(locn0)]];",
        "float4 loc2 [[user(locn2), flat]];",
        " == 0u ? in.loc0 : ",
        " == 2u ? in.loc2 : float4(0.0f))",
    ] {
        assert!(m.contains(want), "missing {want}\n{m}");
    }
    assert!(!m.contains("loc1"), "{m}");
}

#[test]
fn a_constant_index_names_its_member() {
    let m = msl(&indexed_pixel(Some(2), 1));
    assert!(m.contains("in.loc2.y"), "{m}");
    let m = msl(&indexed_pixel(Some(1), 0));
    assert!(m.contains("float4(0.0f).x"), "{m}");
}

#[test]
fn a_register_index_stores_into_one_of_sixteen_outputs() {
    let m = msl(&indexed_vertex(None, 3));
    assert!(m.contains("float4 loc15 [[user(locn15)]];"), "{m}");
    assert!(m.contains(" == 14u ? out.loc14 : out.loc15).w = "), "{m}");
}

#[test]
fn indexed_vertex_inputs_are_the_float_attributes() {
    let b = indexed_vertex(None, 0);
    let m = msl(&b);
    assert!(m.contains("float4 loc0 [[attribute(0)]];"), "{m}");
    assert!(m.contains("float4 loc2 [[attribute(2)]];"), "{m}");
    assert!(!m.contains("loc1 [[attribute"), "{m}");
    assert!(
        notes(&b)
            .iter()
            .any(|n| n.starts_with("indexed vertex attributes"))
    );
}

fn vertex_with(f: impl Fn(&mut B, A)) -> B {
    let mut b = B::new(Stage::Vertex, "vs");
    b.s.props.definitions.attribute_types = vec![0];
    b.io(StorageKind::Output, IoVariable::Position, 0);
    let x = b.k(1);
    f(&mut b, x);
    let r = b.op(Inst::Return, StorageKind::None, 0, vec![]);
    b.put(r);
    b
}

#[test]
fn a_viewport_mask_becomes_the_lowest_viewport() {
    let b = vertex_with(|b, x| {
        b.io(StorageKind::Output, IoVariable::ViewportMask, 0);
        let src = vec![b.k(IoVariable::ViewportMask.0 as i32), b.k(0), x];
        let st = b.op(Inst::Store, StorageKind::Output, 0, src);
        b.put(st);
    });
    let m = msl(&b);
    assert!(
        m.contains("uint gl_ViewportIndex [[viewport_array_index]];"),
        "{m}"
    );
    assert!(
        m.contains("out.gl_ViewportIndex = min(ctz(uint(1)), 15u);"),
        "{m}"
    );
    assert!(notes(&b).iter().any(|n| n.starts_with("the viewport mask")));
}

#[test]
fn a_mask_and_an_index_share_one_member() {
    let b = vertex_with(|b, x| {
        b.io(StorageKind::Output, IoVariable::ViewportMask, 0);
        b.io(StorageKind::Output, IoVariable::ViewportIndex, 0);
        let src = vec![b.k(IoVariable::ViewportMask.0 as i32), b.k(0), x];
        let st = b.op(Inst::Store, StorageKind::Output, 0, src);
        b.put(st);
        b.store_io(IoVariable::ViewportIndex, None, 0, x);
    });
    let m = msl(&b);
    assert_eq!(m.matches("[[viewport_array_index]]").count(), 1, "{m}");
}

#[test]
fn a_draw_index_is_zero() {
    let b = vertex_with(|b, _| {
        b.io(StorageKind::Input, IoVariable::DrawIndex, 0);
        let v = b.load_scalar(IoVariable::DrawIndex, AggType::U32);
        b.store_io(IoVariable::Position, None, 1, v);
    });
    let m = msl(&b);
    assert!(m.contains("0u"), "{m}");
    assert!(notes(&b).iter().any(|n| n.starts_with("draw index")));
}
