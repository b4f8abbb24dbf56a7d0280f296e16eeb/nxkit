use super::*;

include!("../../../nxb-spirv/tests/data/modules.rs");

fn shader(bytes: &[u8]) -> (vk_table::Key, Shader) {
    let key = nxb_spirv::key_of(bytes);
    let s = nxb_spirv::translate(
        bytes,
        &vk_table::function(&key),
        &vk_table::dual_function(&key),
    );
    match s {
        Ok(Outcome::Shader(s)) => (key, s),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rows_go_into_the_table() {
    let mut rows: Vec<Row> = [(VERT, "/a.vert"), (FRAG, "/a.frag")]
        .iter()
        .map(|(b, n)| {
            let (key, s) = shader(b);
            row(key, &s, n).unwrap()
        })
        .collect();
    assert_eq!(rows[0].stage, vk_table::Stage::Vert);
    let tokens: Vec<String> = rows[0].resources.iter().map(Resource::to_string).collect();
    assert_eq!(tokens.join(" "), "u:0.1=0 p:0.0=1");
    let text = vk_table::write(&rows, &nxb_spirv::translator_id());
    assert!(text.contains("# nxb-spirv 2, naga 30.0.1\n"), "{text}");
    let mut back = vk_table::parse(&text).unwrap();
    back.sort_by_key(|r| r.source.clone());
    rows.sort_by_key(|r| r.source.clone());
    assert_eq!(back, rows);
    // the function names of the MSL are the table's
    let (key, s) = shader(FRAG);
    assert!(s.msl.contains(&format!(" {}(", vk_table::function(&key))));
    assert!(
        s.msl_dual
            .unwrap()
            .contains(&format!(" {}(", vk_table::dual_function(&key)))
    );
}

#[test]
fn a_resource_outside_the_table_limits_is_an_error() {
    let (key, mut s) = shader(VERT);
    s.resources[0].binding = vk_table::BINDINGS;
    assert!(
        row(key, &s, "/a")
            .unwrap_err()
            .contains("outside the limits")
    );
}

#[test]
fn find_takes_the_spirv_files_only() {
    let d = std::env::temp_dir().join(format!("nxb-vk-find-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("Content/Fx")).unwrap();
    std::fs::write(d.join("Content/Fx/a.vert"), VERT).unwrap();
    std::fs::write(d.join("Content/b.frag"), FRAG).unwrap();
    // not a module: other magic, too short, a size that is no multiple of 4
    std::fs::write(d.join("Content/Fx/x.bin"), [0u8; 64]).unwrap();
    std::fs::write(d.join("short"), &VERT[..8]).unwrap();
    std::fs::write(d.join("odd"), &VERT[..VERT.len() - 1]).unwrap();
    let f = find(&d).unwrap();
    let names: Vec<&str> = f.iter().map(|x| x.0.as_str()).collect();
    assert_eq!(names, ["/Content/Fx/a.vert", "/Content/b.frag"]);
    std::fs::remove_dir_all(&d).unwrap();
}

/// Translates, compiles and links the vertex and the fragment module (with its dual-source variant) with Xcode's tools.
/// Skipped where they are missing.
#[test]
fn modules_compile_with_the_mac_convention_and_link() {
    let tmp = std::env::temp_dir().join(format!("nxb-vk-compile-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let tools = match MetalTools::for_target(
        &Resolver::from_env(),
        MAC.target,
        tmp.clone(),
        Arc::new(ProcessRegistry::new()),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let mut airs = Vec::new();
    for (bytes, n) in [(VERT, "/a.vert"), (FRAG, "/a.frag")] {
        match one(&tools, nxb_spirv::key_of(bytes), bytes, n) {
            Made::Done(r, a) => {
                assert_eq!(r.source, n);
                airs.extend(a);
            }
            Made::Skipped(s) => panic!("skipped {s}"),
            Made::Failed(e) => panic!("{e}"),
        }
    }
    assert_eq!(airs.len(), 3);
    let (lib, _) = tools.link_many(&airs).unwrap();
    assert_eq!(&lib[..4], b"MTLB");
    let _ = std::fs::remove_dir_all(&tmp);
}
