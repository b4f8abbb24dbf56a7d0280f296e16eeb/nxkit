//! Tests for the resolution of seeds against a library folder.

use super::*;

const V: &str = "00000000000000a1";
const F: &str = "00000000000000b2";

fn lib(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxb-seeded-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join(format!("{V}_b.json")),
        r#"{"entry": "main0", "entry_z": "main0_z", "vertex_inputs": [{"location": 3, "msl_type": "float4"}]}"#,
    )
    .unwrap();
    std::fs::write(
        d.join(format!("{F}_b.json")),
        r#"{"entry": "main0", "raster": {"colors": [0]}}"#,
    )
    .unwrap();
    d
}

fn seeds(depth: &str) -> Seeds {
    let text = format!(
        "nxkit-pipeline-seeds 1\n{depth}S\t{V}\nC\t{F}\nR\t{V}\t{F}\t1\t0\tRgba16Float:f:5.6.1.2.6.1\t3:22:12:1\t1:24:0\nR\t{V}\t00000000000000ff\t1\t0\t-\t-\t-\n"
    );
    read_seeds(&[&text]).0
}

#[test]
fn a_seed_resolves_with_the_facts_of_the_library_folder() {
    let d = lib("one");
    let (entries, n) = Resolver::new(&d).resolve(&seeds("D\tzero\n"), |_| true);
    assert_eq!(
        n,
        Counts {
            resolved: 2,
            no_shader: 1,
            refused: 0
        }
    );
    let Entry::Render(r) = &entries[1] else {
        panic!("render")
    };
    assert_eq!(r.vertex, (format!("{V}_b"), "main0_z".to_string()));
    assert_eq!(r.layout.attributes.len(), 1);
    let exact = crate::exact::ExactState::from_render(r).unwrap();
    assert_eq!(exact.colors[0].as_ref().unwrap().format, "RGBA16Float");
    assert_eq!(exact.attributes[0].1, "Float3");
}

#[test]
fn without_a_depth_mode_both_are_resolved() {
    let d = lib("both");
    let (entries, _) = Resolver::new(&d).resolve(&seeds(""), |s| matches!(s, Seed::Render(_)));
    assert_eq!(entries.len(), 2, "main0 and main0_z");
    assert_eq!(pairs(&seeds("")).len(), 2);
    assert_eq!(computes(&seeds("")), [format!("{F}_b")]);
    assert_eq!(shader_order(&seeds("")), [F, V, "00000000000000ff"]);
}
