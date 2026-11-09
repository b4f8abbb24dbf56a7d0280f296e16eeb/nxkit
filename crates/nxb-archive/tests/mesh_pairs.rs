//! A mesh shader's pairs are left out of the archive plan: the host builds mesh pipelines at the draw. A vertex shader
//! that pulls its inputs is planned with its `_bp` library.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nxb_archive::plan::plan;
use std::fs;
use std::path::PathBuf;

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("mesh_pairs");
    fs::create_dir_all(&d).unwrap();
    d
}

fn reflection(d: &std::path::Path, key: &str, extra: &str) {
    let text = format!("{{\"chain\": \"abc\", \"entry\": \"main0\"{extra}}}");
    fs::write(d.join(format!("{key}_b.json")), text).unwrap();
}

#[test]
fn a_mesh_pair_is_counted_and_not_planned() {
    let d = dir();
    reflection(&d, "ms", ", \"mesh_stage\": {\"kind\": \"ms\"}");
    reflection(&d, "vs", "");
    reflection(&d, "ps", "");
    let pairs = [
        ("ms".to_string(), "ps".to_string()),
        ("vs".to_string(), "ps".to_string()),
    ];
    let p = plan(&d, "abc", &pairs, false);
    assert_eq!(p.left[1], 1);
    // The vertex pair goes on to the library check, which fails here: there is no metallib.
    assert_eq!(p.left[3], 1);
    assert!(p.bases.is_empty());
}

fn tag(out: &mut Vec<u8>, t: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(t);
    out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    out.extend_from_slice(data);
}

/// A metallib with only the tables `functions` reads: the entry names, none with function constants.
fn metallib(entries: &[&str]) -> Vec<u8> {
    let mut names = (entries.len() as u32).to_le_bytes().to_vec();
    let mut data = Vec::new();
    for n in entries {
        let mut t = Vec::new();
        tag(&mut t, b"NAME", n.as_bytes());
        t.extend_from_slice(b"ENDT");
        names.extend_from_slice(&(t.len() as u32 + 4).to_le_bytes());
        names.extend_from_slice(&t);
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(b"ENDT");
    }
    let fo = 0x58usize;
    let po = fo + names.len();
    let mut b = vec![0u8; fo];
    b[..4].copy_from_slice(b"MTLB");
    b[0x18..0x20].copy_from_slice(&(fo as u64).to_le_bytes());
    b[0x28..0x30].copy_from_slice(&(po as u64).to_le_bytes());
    b[0x30..0x38].copy_from_slice(&(data.len() as u64).to_le_bytes());
    b.extend_from_slice(&names);
    b.extend_from_slice(&data);
    b
}

/// The host draws a vertex shader with a `pull` field through its `_bp` library (`nxkit-gpu` `pull`), whose
/// pipelines have no vertex layout: the plan archives exactly that function, also for a shader with stage inputs.
#[test]
fn a_vertex_shader_that_pulls_is_archived_with_its_bp_library() {
    let d = dir().join("pull");
    fs::create_dir_all(&d).unwrap();
    reflection(
        &d,
        "vs",
        concat!(
            ", \"vertex_inputs\": [0], \"pull\": ",
            "{\"entry\": \"main0\", \"entry_z\": \"main0_z\", \"slot\": 16, \"inputs\": [0]}"
        ),
    );
    reflection(&d, "ps", "");
    fs::write(
        d.join("vs_b.metallib"),
        metallib(&["main0", "main0_z", "main0_tf"]),
    )
    .unwrap();
    fs::write(d.join("vs_bp.metallib"), metallib(&["main0", "main0_z"])).unwrap();
    fs::write(d.join("ps_b.metallib"), metallib(&["main0"])).unwrap();
    let p = plan(&d, "abc", &[("vs".to_string(), "ps".to_string())], false);
    assert_eq!(p.left, [0; 7]);
    assert_eq!(
        p.bases,
        vec![(
            "vs_bp".to_string(),
            "main0".to_string(),
            "ps_b".to_string(),
            "main0".to_string()
        )]
    );
}

/// A vertex shader without inputs keeps its `_b` library; one with inputs that cannot pull (no `pull` field, or one at
/// another slot) needs the vertex layout of the draw and is left out.
#[test]
fn a_vertex_shader_with_inputs_that_cannot_pull_is_left_out() {
    let d = dir().join("nopull");
    fs::create_dir_all(&d).unwrap();
    reflection(&d, "plain", "");
    reflection(&d, "inputs", ", \"vertex_inputs\": [0]");
    reflection(
        &d,
        "slot",
        ", \"vertex_inputs\": [0], \"pull\": {\"slot\": 17}",
    );
    reflection(&d, "ps", "");
    for k in ["plain", "inputs", "slot"] {
        fs::write(d.join(format!("{k}_b.metallib")), metallib(&["main0"])).unwrap();
        fs::write(d.join(format!("{k}_bp.metallib")), metallib(&["main0"])).unwrap();
    }
    fs::write(d.join("ps_b.metallib"), metallib(&["main0"])).unwrap();
    let pairs: Vec<(String, String)> = ["plain", "inputs", "slot"]
        .iter()
        .map(|v| (v.to_string(), "ps".to_string()))
        .collect();
    let p = plan(&d, "abc", &pairs, false);
    assert_eq!(p.left[2], 2);
    assert_eq!(
        p.bases,
        vec![(
            "plain_b".to_string(),
            "main0".to_string(),
            "ps_b".to_string(),
            "main0".to_string()
        )]
    );
}
