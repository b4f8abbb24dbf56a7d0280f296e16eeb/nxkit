use super::*;
use crate::seen::HEADER;

fn line(key: u32, topology: &str, samples: u32, a2c: u8, colors: &str, layout: &str) -> String {
    format!(
        "R\t{key:016x}\t00000000000000bb\t0\tv1_b\tmain0\tf1_b\tmain0\t0\t{topology}\t{samples}\t{a2c}\t{colors}\t{layout}"
    )
}

fn list(lines: &[String]) -> String {
    format!("{HEADER}\n{}\n", lines.join("\n"))
}

fn desc(s: &ExactState) -> Value {
    s.descriptor(Value::str("fnd:v"), Value::str("fnd:f"))
}

fn member<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k)
}

#[test]
fn every_colour_state_of_a_pair_is_a_state_of_its_own() {
    let t = list(&[
        line(1, "Triangle", 1, 0, "Rg11B10Float:f:-", "-"),
        line(2, "Triangle", 1, 0, "Rgba16Float:f:-", "-"),
        line(3, "Triangle", 1, 0, "Rg11B10Float:7:-", "-"),
        // the same descriptor under another key (the debug flags of the key) is one state
        line(4, "Triangle", 1, 0, "Rg11B10Float:f:-", "-"),
    ]);
    let s = read_list(&t);
    assert_eq!(s.len(), 3);
    assert_eq!(s[0].colors[0].as_ref().unwrap().format, "RG11B10Float");
    assert_eq!(s[1].colors[0].as_ref().unwrap().format, "RGBA16Float");
    assert_eq!(s[2].colors[0].as_ref().unwrap().mask, 7);
}

#[test]
fn a_plain_state_is_exact_too() {
    // the bases of `seen` leave it to the plain base; the exact list holds it
    let s = read_list(&list(&[line(1, "Triangle", 1, 0, "Bgra8Unorm:f:-", "-")]));
    assert_eq!(s.len(), 1);
    let d = desc(&s[0]);
    assert_eq!(
        member(&d, "input_primitive_topology").and_then(Value::as_str),
        Some("Triangle")
    );
    assert!(member(&d, "vertex_descriptor").is_none());
    assert!(member(&d, "raster_sample_count").is_none());
}

#[test]
fn the_descriptor_carries_everything_the_host_sets() {
    let blend = "SrcAlpha/OneMinusSrcAlpha/Add/One/OneMinusSrcAlpha/Max";
    let colors = format!("Rgba16Float:5:{blend},_,Rgba8Unorm:f:-");
    let s = read_list(&list(&[line(
        1,
        "Line",
        4,
        1,
        &colors,
        "a0.Float3.0.0,l0.12.PerVertex.1",
    )]));
    let d = desc(&s[0]);
    assert_eq!(member(&d, "raster_sample_count"), Some(&Value::Int(4)));
    assert_eq!(
        member(&d, "alpha_to_coverage_state").and_then(Value::as_str),
        Some("Enabled")
    );
    assert!(member(&d, "vertex_descriptor").is_some());
    let list = member(&d, "color_attachments").unwrap().as_list().unwrap();
    assert_eq!(list.len(), 3);
    let a = &list[0];
    assert_eq!(
        a.get("pixel_format").and_then(Value::as_str),
        Some("RGBA16Float")
    );
    assert_eq!(
        a.get("write_mask").and_then(Value::as_str),
        Some("Red Blue")
    );
    assert_eq!(
        a.get("blending_state").and_then(Value::as_str),
        Some("Enabled")
    );
    assert_eq!(
        a.get("source_rgb_blend_factor").and_then(Value::as_str),
        Some("SourceAlpha")
    );
    assert_eq!(
        a.get("destination_rgb_blend_factor")
            .and_then(Value::as_str),
        Some("OneMinusSourceAlpha")
    );
    assert_eq!(
        a.get("alpha_blend_operation").and_then(Value::as_str),
        Some("Max")
    );
    // an empty slot is an empty object, a slot without a blend and a full mask has the format alone
    assert_eq!(list[1], Value::Object(Vec::new()));
    assert_eq!(
        list[2],
        Value::Object(vec![("pixel_format".into(), Value::str("RGBA8Unorm"))])
    );
}

#[test]
fn a_state_without_colours_has_no_attachment_list() {
    let s = read_list(&list(&[line(1, "Triangle", 1, 0, "-", "-")]));
    assert!(member(&desc(&s[0]), "color_attachments").is_none());
}

#[test]
fn the_write_mask_is_named_by_its_channels() {
    assert_eq!(mask_text(0xF), "All");
    assert_eq!(mask_text(0), "None");
    assert_eq!(mask_text(0x8), "Alpha");
    assert_eq!(mask_text(0x7), "Red Green Blue");
}

#[test]
fn lines_the_script_cannot_name_are_left_out() {
    let t = list(&[
        // a format the script has no name for
        line(1, "Triangle", 1, 0, "NoFormat:f:-", "-"),
        // a second source alpha factor
        line(
            2,
            "Triangle",
            1,
            0,
            "Rgba8Unorm:f:Src1Alpha/One/Add/One/One/Add",
            "-",
        ),
        line(3, "Cone", 1, 0, "-", "-"),
        "R\tbroken".to_string(),
        // a depth only pipeline has no fragment stage
        "R\t0000000000000005\t00000000000000bb\t0\tv1_b\tmain0\t-\t-\t0\tTriangle\t1\t0\t-\t-"
            .to_string(),
        "M\t1\t-\t-\tm_b\tmain0\tf_b\tmain0\t1\t0\t0\t0\t0\t-".to_string(),
        line(6, "Triangle", 1, 0, "Rgba8Unorm:f:-", "-"),
    ]);
    assert_eq!(read_list(&t).len(), 1);
    // a text of another version gives none
    assert!(read_list(&t.replacen("seen 2", "seen 3", 1)).is_empty());
}

#[test]
fn every_host_format_has_a_script_name() {
    // the names the list uses are `Debug` of the host's formats; the table must hold a name for each colour format
    for (host, script) in PIXEL_FORMATS {
        assert!(!host.is_empty() && !script.is_empty());
        assert_eq!(pixel_format(host), Some(*script));
    }
    assert_eq!(pixel_format("Rgba8UnormSrgb"), Some("RGBA8Unorm_sRGB"));
    assert_eq!(pixel_format("Rg11B10Float"), Some("RG11B10Float"));
    assert_eq!(PIXEL_FORMATS.len(), 47);
}

#[test]
fn a_state_the_compiler_would_refuse_is_named() {
    let bad = read_list(&list(&[line(
        1,
        "Triangle",
        1,
        0,
        "Rgba8Unorm:f:-",
        "a0.Int1010102Normalized.12.0,l0.8.PerVertex.1",
    )]));
    let reason = bad[0].invalid(None).unwrap();
    assert!(reason.contains("does not fit the stride"), "{reason}");
    let odd = read_list(&list(&[line(1, "Triangle", 3, 0, "Rgba8Unorm:f:-", "-")]));
    assert!(odd[0].invalid(None).unwrap().contains("sample count"));
}

#[test]
fn the_pair_is_named_by_its_shader_keys() {
    let s = read_list(&list(&[line(1, "Triangle", 1, 0, "-", "-")]));
    assert_eq!(s[0].pair_keys(), Some(("v1".to_string(), "f1".to_string())));
}
