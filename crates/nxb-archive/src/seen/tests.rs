use super::*;
use crate::plan::{script_text_all, script_text_with};
use crate::script::Script;

const COLORS: &str = "Bgra8Unorm:f:-";

fn line(fallback: u8, icb: u8, topology: &str, samples: u32, a2c: u8, layout: &str) -> String {
    format!(
        "R\t00000000000000aa\t00000000000000bb\t{fallback}\tv1_b\tmain0\tf1_b\tmain0\t{icb}\t{topology}\t{samples}\t{a2c}\t{COLORS}\t{layout}"
    )
}

fn list(lines: &[String]) -> String {
    format!("{HEADER}\n{}\n", lines.join("\n"))
}

#[test]
fn a_plain_state_needs_no_base_of_its_own() {
    let t = list(&[line(0, 0, "Triangle", 1, 0, "-")]);
    assert!(read_list(&t).is_empty());
}

#[test]
fn a_vertex_layout_samples_alpha_to_coverage_or_icb_make_a_base() {
    let t = list(&[
        line(
            0,
            0,
            "Triangle",
            1,
            0,
            "a0.Float3.0.0,a1.UChar4Norm.12.0,l0.16.PerVertex.1",
        ),
        line(0, 0, "Triangle", 4, 0, "-"),
        line(0, 0, "Triangle", 1, 1, "-"),
        line(0, 1, "Unspecified", 1, 0, "-"),
    ]);
    let b = read_list(&t);
    assert_eq!(b.len(), 4);
    assert_eq!(b[0].vertex, ("v1_b".to_string(), "main0".to_string()));
    assert_eq!(
        b[0].attributes,
        vec![
            (0, "Float3".to_string(), 0, 0),
            (1, "UChar4Normalized".to_string(), 12, 0)
        ]
    );
    assert_eq!(b[0].layouts, vec![(0, 16, "PerVertex".to_string(), 1)]);
    assert_eq!(b[1].samples, 4);
    assert!(b[2].alpha_to_coverage);
    assert!(b[3].icb);
}

#[test]
fn the_same_state_with_other_colours_is_one_base() {
    let a = line(0, 0, "Triangle", 4, 0, "-");
    let b = a.replace(COLORS, "Rgba16Float:f:-");
    assert_eq!(read_list(&list(&[a, b])).len(), 1);
}

#[test]
fn a_fallback_state_also_gets_the_base_without_a_class() {
    let b = read_list(&list(&[line(1, 0, "Line", 4, 0, "-")]));
    assert_eq!(
        b.iter().map(|b| b.topology.as_str()).collect::<Vec<_>>(),
        ["Line", "Unspecified"]
    );
}

#[test]
fn lines_that_do_not_parse_are_left_out() {
    let t = list(&[
        line(0, 0, "Triangle", 4, 0, "a0.NoSuchFormat.0.0"),
        line(0, 0, "Cone", 4, 0, "-"),
        line(0, 0, "Triangle", 4, 0, "a0.FloatRg11B10.0.0"),
        "R\tbroken".to_string(),
        "M\t1\t-\t-\tm_b\tmain0\tf_b\tmain0\t1\t0\t0\t0\t0\t-".to_string(),
        line(0, 0, "Triangle", 4, 0, "-"),
    ]);
    assert_eq!(read_list(&t).len(), 1);
    // a list of another version is not read
    assert!(read_list(&t.replace("seen 2", "seen 3")).is_empty());
    assert!(read_list("").is_empty());
}

#[test]
fn the_host_names_of_the_vertex_formats_become_the_script_names() {
    assert_eq!(format_name("Float2").as_deref(), Some("Float2"));
    assert_eq!(
        format_name("UChar4Norm").as_deref(),
        Some("UChar4Normalized")
    );
    assert_eq!(
        format_name("UChar4NormBgra").as_deref(),
        Some("UChar4Normalized_BGRA")
    );
    assert_eq!(
        format_name("Int1010102Norm").as_deref(),
        Some("Int1010102Normalized")
    );
    assert_eq!(format_name("FloatRg11B10"), None);
}

fn script_of(states: &[StateBase]) -> Script {
    let dir = std::env::temp_dir();
    Script::parse(&script_text_all(&dir, &[], states, &[])).expect("the script parses")
}

#[test]
fn the_script_gets_the_state_and_unspecialized_colours() {
    let t = list(&[line(
        0,
        1,
        "Triangle",
        4,
        1,
        "a0.Float3.0.0,a2.Half2.8.1,l0.12.PerVertex.1,l1.4.Constant.1",
    )]);
    let s = script_of(&read_list(&t));
    assert_eq!(s.pipelines.len(), 1);
    let p = &s.pipelines[0];
    let get = |k: &str| nxb_pyjson::dumps(p.get(k).expect(k));
    assert_eq!(get("input_primitive_topology"), "\"Triangle\"");
    assert_eq!(get("raster_sample_count"), "4");
    assert_eq!(get("alpha_to_coverage_state"), "\"Enabled\"");
    assert_eq!(get("support_indirect_command_buffers"), "\"Enabled\"");
    let colours = p
        .get("color_attachments")
        .and_then(Value::as_list)
        .expect("colours");
    assert_eq!(colours.len(), 8);
    assert!(nxb_pyjson::dumps(&colours[7]).contains("\"pixel_format\": \"Unspecialized\""));
    let vd = p.get("vertex_descriptor").expect("vertex descriptor");
    let attrs = vd
        .get("attributes")
        .and_then(Value::as_list)
        .expect("attributes");
    // slot 1 nobody sets stays invalid
    assert_eq!(attrs.len(), 3);
    assert!(nxb_pyjson::dumps(&attrs[1]).contains("Invalid"));
    assert!(nxb_pyjson::dumps(&attrs[2]).contains("\"format\": \"Half2\""));
    assert!(nxb_pyjson::dumps(&attrs[2]).contains("\"buffer_index\": 1"));
    let layouts = vd.get("layouts").and_then(Value::as_list).expect("layouts");
    assert_eq!(layouts.len(), 2);
    // the host gives a constant step no rate
    assert!(nxb_pyjson::dumps(&layouts[1]).contains("\"step_rate\": 0"));
    assert!(nxb_pyjson::dumps(&layouts[0]).contains("\"stride\": 12"));
}

#[test]
fn a_state_without_a_vertex_layout_has_no_vertex_descriptor() {
    let s = script_of(&read_list(&list(&[line(0, 0, "Unspecified", 4, 0, "-")])));
    let p = &s.pipelines[0];
    assert!(p.get("vertex_descriptor").is_none());
    assert!(p.get("input_primitive_topology").is_none());
    assert!(p.get("alpha_to_coverage_state").is_none());
}

#[test]
fn no_states_leave_the_script_as_it_was() {
    let dir = std::env::temp_dir();
    let bases = vec![(
        "v_b".to_string(),
        "main0".to_string(),
        "f_b".to_string(),
        "main0".to_string(),
    )];
    assert_eq!(
        script_text_all(&dir, &bases, &[], &[]),
        script_text_with(&dir, &bases, &[])
    );
}

#[test]
fn the_pair_keys_come_from_the_stems() {
    let b = read_list(&list(&[line(0, 0, "Triangle", 4, 0, "-")]));
    assert_eq!(b[0].pair_keys(), Some(("v1".to_string(), "f1".to_string())));
    let mut c = b[0].clone();
    c.vertex.0 = "v1_bp".into();
    assert_eq!(c.pair_keys(), Some(("v1".to_string(), "f1".to_string())));
    c.vertex.0 = "other".into();
    assert_eq!(c.pair_keys(), None);
}
