//! Tests for the seed text and the resolution of a seed into the entry of a draw.

use std::collections::HashMap;

use super::*;
use crate::list::Seen;
use crate::program::MeshFacts;
use crate::state::VertexFormat;

const V: &str = "00000000000000a1";
const F: &str = "00000000000000b2";
const M: &str = "00000000000000c3";

fn sample() -> Seeds {
    let mut s = Seeds::default();
    s.insert(Seed::Shader(V.into()));
    s.insert(Seed::Compute(F.into()));
    s.insert(Seed::Render(RenderSeed {
        vertex: V.into(),
        fragment: Some(F.into()),
        samples: 4,
        a2c: false,
        colors: vec![
            Some(ColorSeed {
                format: Format::Rgba16Float,
                mask: 0xF,
                blend: Some(Blend {
                    enable: true,
                    src_rgb: 5,
                    dst_rgb: 6,
                    op_rgb: 1,
                    src_alpha: 2,
                    dst_alpha: 6,
                    op_alpha: 1,
                }),
            }),
            None,
            Some(ColorSeed {
                format: Format::R32Uint,
                mask: 1,
                blend: None,
            }),
            None,
        ],
        attributes: vec![Attribute {
            location: 0,
            format: 0x25 + 9,
            offset: 0,
            stream: 0,
        }],
        streams: vec![Stream {
            index: 0,
            stride: 16,
            divisor: 0,
        }],
    }));
    s.insert(Seed::Mesh(MeshSeed {
        task: None,
        mesh: M.into(),
        fragment: Some(F.into()),
        samples: 1,
        a2c: true,
        colors: vec![Some(ColorSeed {
            format: Format::Rgba8Unorm,
            mask: 0xF,
            blend: None,
        })],
    }));
    s
}

#[test]
fn the_seeds_round_trip_through_their_text() {
    let s = sample();
    let text = s.to_text(&["from a test".into()]);
    assert!(text.starts_with(HEADER));
    assert!(text.contains("\n# from a test\n"));
    assert_eq!(Seeds::parse(&text).unwrap(), s);
    assert!(Seeds::parse("nxkit-pipeline-seeds 2\n").is_err());
    assert!(parse_line("S\tnot-a-key").is_none());
    assert!(parse_line("R\tx").is_none());
}

fn facts() -> HashMap<(&'static str, bool), ProgramFacts> {
    let mut m = HashMap::new();
    m.insert(
        (V, true),
        ProgramFacts {
            entry: "main0".into(),
            entry_z: [Some("main0_z".into()), None],
            inputs: vec![(0, VertexFormat::Float4)],
            pull: Some("main0".into()),
            ..Default::default()
        },
    );
    let frag = ProgramFacts {
        entry: "main0".into(),
        entry_mixed: Some(("main0_m".into(), 0b100)),
        colors: Some(vec![0, 2]),
        ..Default::default()
    };
    m.insert((F, false), frag);
    m.insert(
        (M, false),
        ProgramFacts {
            entry: "main0".into(),
            entry_z: [Some("main0_z".into()), None],
            mesh: Some(MeshFacts {
                kind: crate::program::MeshKind::Mesh,
                threads: 32,
                payload: 0,
                task: false,
            }),
            ..Default::default()
        },
    );
    m
}

#[test]
fn a_render_seed_resolves_to_the_entry_of_the_draw() {
    let f = facts();
    let get = |k: &str, v: bool| {
        f.iter()
            .find(|((fk, fv), _)| *fk == k && *fv == v)
            .map(|(_, p)| p)
    };
    let s = sample();
    let e = resolve(
        &s.seeds()[2],
        &get,
        Drawing {
            zero_to_one: true,
            pull: false,
        },
    )
    .unwrap()
    .unwrap();
    let Entry::Render(r) = &e else {
        panic!("render")
    };
    assert_eq!(r.vertex, (format!("{V}_b"), "main0_z".to_string()));
    assert_eq!(r.fragment, Some((format!("{F}_b"), "main0_m".to_string())));
    assert_eq!(r.colors.len(), 3, "the slots end at the last target");
    assert!(r.colors[0].unwrap().blend.is_some());
    assert!(
        r.colors[2].unwrap().blend.is_none(),
        "an integer target never blends"
    );
    assert_eq!(r.samples, PASS_SAMPLES);
    assert_eq!(r.layout.attributes.len(), 1);
    assert_eq!(r.key, None);
    // a resolved entry is a line of a list that the host and the archive read
    let mut list = Seen::default();
    list.insert(e.clone());
    assert_eq!(Seen::parse(&list.to_text()).unwrap().entries(), &[e]);

    let pulled = resolve(
        &s.seeds()[2],
        &get,
        Drawing {
            zero_to_one: false,
            pull: true,
        },
    )
    .unwrap()
    .unwrap();
    let Entry::Render(p) = pulled else {
        panic!("render")
    };
    assert_eq!(p.vertex.0, format!("{V}_bp"));
    assert_eq!(p.layout, VertexDescriptor::default());
}

#[test]
fn mesh_compute_and_missing_shaders() {
    let f = facts();
    let get = |k: &str, v: bool| {
        f.iter()
            .find(|((fk, fv), _)| *fk == k && *fv == v)
            .map(|(_, p)| p)
    };
    let s = sample();
    assert_eq!(resolve(&s.seeds()[0], &get, Drawing::default()), Ok(None));
    let Some(Entry::Compute(c)) = resolve(&s.seeds()[1], &get, Drawing::default()).unwrap() else {
        panic!("compute")
    };
    assert_eq!(c.function, (format!("{F}_b"), "main0".to_string()));
    let Some(Entry::Mesh(m)) = resolve(
        &s.seeds()[3],
        &get,
        Drawing {
            zero_to_one: true,
            pull: false,
        },
    )
    .unwrap() else {
        panic!("mesh")
    };
    assert_eq!(m.mesh, (format!("{M}_b"), "main0_z".to_string()));
    assert_eq!(m.mesh_threads, 32);
    let none = |_: &str, _: bool| None;
    assert_eq!(
        resolve(&s.seeds()[1], &none, Drawing::default()),
        Err(Unresolved::Shader(F.into()))
    );
}
