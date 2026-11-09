//! Tests for the list of seen pipelines: the text round trip, the merge and the refusal of damage.

use super::*;

fn blend() -> BlendState {
    BlendState {
        src_rgb: BlendFactor::SrcAlpha,
        dst_rgb: BlendFactor::OneMinusSrcAlpha,
        op_rgb: BlendOp::Add,
        src_alpha: BlendFactor::One,
        dst_alpha: BlendFactor::Zero,
        op_alpha: BlendOp::Max,
    }
}

fn stage(l: &str, e: &str) -> Stage {
    (l.to_string(), e.to_string())
}

fn render(key: u64) -> RenderRec {
    RenderRec {
        key,
        vertex_key: key.wrapping_mul(7),
        fallback: key % 2 == 1,
        vertex: stage("0a1b_b", "main0"),
        fragment: Some(stage("9f_b", "main0_u")),
        colors: vec![
            Some(ColorTarget {
                format: Format::Rgba8Unorm,
                blend: Some(blend()),
                write_mask: 0xF,
            }),
            None,
            Some(ColorTarget::new(Format::R32Uint)),
        ],
        layout: VertexDescriptor {
            attributes: vec![(
                0,
                VertexAttribute {
                    format: VertexFormat::Float3,
                    offset: 12,
                    buffer: 1,
                },
            )],
            layouts: vec![VertexLayout {
                buffer: 1,
                stride: 24,
                step: StepFunction::PerInstance,
                step_rate: 2,
            }],
        },
        topology: TopologyClass::Line,
        samples: 4,
        a2c: true,
        icb: false,
    }
}

fn mesh(key: u64) -> MeshRec {
    MeshRec {
        key,
        object: Some(stage("t_b", "task")),
        mesh: stage("m_b", "mesh"),
        fragment: None,
        colors: vec![Some(ColorTarget::new(Format::Rgba16Float))],
        samples: 1,
        a2c: false,
        object_threads: 32,
        mesh_threads: 128,
        payload: 64,
    }
}

fn sample() -> Seen {
    let mut s = Seen::default();
    s.insert(Entry::Render(render(1)));
    s.insert(Entry::Render(RenderRec {
        fragment: None,
        colors: Vec::new(),
        layout: VertexDescriptor::default(),
        ..render(2)
    }));
    s.insert(Entry::Mesh(mesh(3)));
    s.insert(Entry::Compute(ComputeRec {
        key: 4,
        function: stage("c_b", "kern"),
    }));
    s
}

#[test]
fn the_list_round_trips_through_its_text() {
    let s = sample();
    let text = s.to_text();
    assert!(text.starts_with(HEADER));
    assert_eq!(text.lines().count(), 5);
    assert_eq!(Seen::parse(&text).unwrap(), s);
}

#[test]
fn an_entry_is_kept_once_and_keeps_its_place() {
    let mut s = sample();
    assert!(!s.insert(Entry::Render(RenderRec {
        samples: 8,
        ..render(1)
    })));
    assert_eq!(s.len(), 4);
    assert_eq!(s.entries()[0], Entry::Render(render(1)));
    // the same key of another kind is another pipeline
    assert!(s.insert(Entry::Compute(ComputeRec {
        key: 1,
        function: stage("c_b", "k2"),
    })));
}

#[test]
fn merging_adds_what_the_other_run_saw_after_the_own_entries() {
    let mut a = sample();
    let mut b = Seen::default();
    b.insert(Entry::Render(render(2)));
    b.insert(Entry::Render(render(10)));
    b.insert(Entry::Mesh(mesh(11)));
    assert_eq!(a.merge(&b), 2);
    assert_eq!(a.len(), 6);
    assert_eq!(a.entries()[4], Entry::Render(render(10)));
    // a second merge changes nothing
    assert_eq!(a.merge(&b), 0);
}

#[test]
fn another_version_or_no_header_is_refused() {
    assert!(Seen::parse("").is_err());
    assert!(Seen::parse("nxkit-pipelines-seen 1\n").is_err());
    assert!(Seen::parse("nxkit-pipelines-seen 3\n").is_err());
    assert!(Seen::parse("C\t1\ta\tb\n").is_err());
}

#[test]
fn a_damaged_line_is_left_out_and_the_others_stay() {
    let good = sample().to_text();
    let mut lines: Vec<&str> = good.lines().collect();
    lines.insert(2, "R\tzz\tnot enough");
    lines.insert(4, "C\t0000000000000007\tbad name\tx\ty");
    lines.push("X\t1\t2");
    let s = Seen::parse(&lines.join("\n")).unwrap();
    assert_eq!(s, sample());
}

#[test]
fn a_name_with_white_space_or_an_unknown_format_is_no_entry() {
    let mut line = sample().to_text().lines().nth(1).unwrap().to_string();
    assert!(parse_line(&line).is_some());
    line = line.replace("Rgba8Unorm", "NoSuchFormat");
    assert!(parse_line(&line).is_none());
    assert!(stage_of("a b", "x").is_none());
    assert_eq!(stage_of("-", "-"), Some(None));
}

#[test]
fn a_block_compressed_target_cannot_be_written() {
    let c = [Some(ColorTarget::new(Format::Bc(nxkit_metal::BcKind::Bc7)))];
    assert_eq!(colors_text(&c), None);
    assert_eq!(colors_text(&[]), Some("-".to_string()));
}
