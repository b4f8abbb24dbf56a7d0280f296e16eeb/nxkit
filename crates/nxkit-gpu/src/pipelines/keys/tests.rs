//! Pins of the key scheme: one fixed description of each kind of pipeline and the key it must give. A key function
//! that hashes something else, or in another order, fails here. Then raise `KEY_SCHEME` and update the value.

use nxkit_metal::{
    BlendState, ColorTarget, Format, StepFunction, VertexAttribute, VertexFormat, VertexLayout,
};

use super::*;

const RENDER: u64 = 0x9f5989b4eb4f5b82;
const RENDER_PULL: u64 = 0x08b67e3d7ebfd006;
const MESH: u64 = 0x29093c32fef42c15;
const COMPUTE: u64 = 0x41547895ce9f9bcc;
const POINT_SIZE: u64 = 0x444a238568ff18aa;

fn pinned(what: &str, got: u64, want: u64) {
    assert_eq!(
        got, want,
        "the key of a recorded pipeline changed: raise KEY_SCHEME and update this value ({what}: computed {got:#018x} = {got})"
    );
}

fn targets() -> Targets {
    Targets {
        colors: vec![Some(Format::Rgba8Unorm), None, Some(Format::Rgba8Unorm)],
        samples: 4,
    }
}

fn colors() -> Vec<Option<ColorTarget>> {
    vec![
        Some(ColorTarget::new(Format::Rgba8Unorm).with_blend(BlendState::ALPHA)),
        None,
        Some(ColorTarget {
            write_mask: 0x7,
            ..ColorTarget::new(Format::Rgba8Unorm)
        }),
    ]
}

fn layout() -> VertexDescriptor {
    VertexDescriptor {
        attributes: vec![
            (
                0,
                VertexAttribute {
                    format: VertexFormat::Float3,
                    offset: 0,
                    buffer: 0,
                },
            ),
            (
                1,
                VertexAttribute {
                    format: VertexFormat::Float2,
                    offset: 12,
                    buffer: 0,
                },
            ),
        ],
        layouts: vec![VertexLayout {
            buffer: 0,
            stride: 20,
            step: StepFunction::PerVertex,
            step_rate: 1,
        }],
    }
}

#[test]
fn the_render_key_is_pinned() {
    let (t, c, l) = (targets(), colors(), layout());
    let key = RenderKey {
        vertex: "0123456789abcdef",
        fragment: Some("fedcba9876543210"),
        targets: &t,
        icb: false,
        colors: &c,
        layout: &l,
        pull: false,
        class: TopologyClass::Triangle,
        alpha_to_coverage: true,
        flat: false,
        paint: false,
        paint_dir: false,
    };
    pinned("render", render_key(&key), RENDER);
}

#[test]
fn the_render_key_of_a_pulling_vertex_function_is_pinned() {
    let (t, c) = (targets(), colors());
    let key = RenderKey {
        vertex: "0123456789abcdef",
        fragment: None,
        targets: &t,
        icb: true,
        colors: &c,
        layout: &VertexDescriptor::default(),
        pull: true,
        class: TopologyClass::Point,
        alpha_to_coverage: false,
        flat: false,
        paint: false,
        paint_dir: false,
    };
    pinned("render, pull", render_key(&key), RENDER_PULL);
}

#[test]
fn the_mesh_key_is_pinned() {
    let (t, c) = (targets(), colors());
    let key = MeshKey {
        task: Some("00112233445566aa"),
        mesh: Some("8899aabbccddeeff"),
        fragment: Some("fedcba9876543210"),
        fragment_entry: Some("main_fs"),
        targets: &t,
        colors: &c,
        alpha_to_coverage: false,
    };
    pinned("mesh", mesh_key(&key), MESH);
}

#[test]
fn the_compute_key_is_pinned() {
    pinned("compute", compute_key("0123456789abcdef"), COMPUTE);
}

#[test]
fn the_point_size_key_is_pinned() {
    pinned(
        "point size",
        point_size_key("0123456789abcdef", "main_vs"),
        POINT_SIZE,
    );
}

#[test]
fn the_keys_tell_their_inputs_apart() {
    let (t, c, l) = (targets(), colors(), layout());
    let base = RenderKey {
        vertex: "a",
        fragment: Some("b"),
        targets: &t,
        icb: false,
        colors: &c,
        layout: &l,
        pull: false,
        class: TopologyClass::Triangle,
        alpha_to_coverage: false,
        flat: false,
        paint: false,
        paint_dir: false,
    };
    let key = render_key(&base);
    assert_eq!(key, render_key(&base));
    let other = RenderKey { icb: true, ..base };
    assert_ne!(key, render_key(&other));
    assert_ne!(compute_key("a"), compute_key("b"));
    assert_ne!(compute_key("a"), point_size_key("a", ""));
}
