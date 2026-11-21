#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use naga::{Binding, Interpolation};

#[test]
fn array_varyings_become_one_float_varying_per_element_with_interpolation() {
    let mut s = crate::spirv::Spv::parse(crate::tests::FRAG).unwrap();
    crate::spirv::split_combined(&mut s).unwrap();
    let (m, _) = crate::to_ir(&s.words(), false, "f").unwrap();
    let args = &m.entry_points[0].function.arguments;
    let locs: Vec<(u32, Option<Interpolation>)> = args
        .iter()
        .filter_map(|a| match a.binding {
            Some(Binding::Location {
                location,
                interpolation,
                ..
            }) => Some((location, interpolation)),
            _ => None,
        })
        .collect();
    assert_eq!(
        locs,
        vec![
            (0, Some(Interpolation::Perspective)),
            (1, Some(Interpolation::Perspective))
        ]
    );
}

#[test]
fn array_outputs_of_a_vertex_shader_are_split() {
    let s = crate::spirv::Spv::parse(crate::tests::VERT).unwrap();
    let (m, _) = crate::to_ir(&s.words(), true, "v").unwrap();
    let ep = &m.entry_points[0].function;
    let ty = ep.result.as_ref().unwrap().ty;
    let naga::TypeInner::Struct { ref members, .. } = m.types[ty].inner else {
        panic!("a struct result");
    };
    let locs: Vec<u32> = members
        .iter()
        .filter_map(|mb| match mb.binding {
            Some(Binding::Location { location, .. }) => Some(location),
            _ => None,
        })
        .collect();
    assert_eq!(locs, vec![0, 1]);
}
