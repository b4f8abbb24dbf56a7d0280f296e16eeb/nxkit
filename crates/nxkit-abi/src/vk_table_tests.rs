#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;

/// Rows of the old engine's table for Hades (`vk_shaders.py` with spirv-cross), with its two comment lines.
const REFERENCE: &str = "# Vulkan shaders (dm/gfx/tools/vk_shaders.py): key = sha256(SPIR-V)[:16], Metal function vk_<key>; resources kind:set.binding=slot (u uniform buffer, b storage buffer, t image, s sampler, c combined, p push constants)
# spirv-cross Git commit: vulkan-sdk-1.4.363.0 Timestamp: 2026-09-16T11:27:19
080e48b1582b0020\tfrag\ts:0.24=0 u:1.37=0 t:2.0=0\t/Content/EffectsForgeSwitch/Shroud2.frag
0fcca318097ae0a2\tvert\tu:1.47=0\t/Content/EffectsForgeSwitch/AlphaMask.vert
38b3482345b3accc\tfrag\ts:0.24=0 t:1.25=0 t:1.26=1 t:1.31=3 u:1.32=0 u:1.46=1 t:2.0=4 t:2.1=5\t/Content/EffectsForgeSwitch/Phantom.frag
7def72ad068c9c8b\tvert\tu:1.44=0 u:1.47=1\t/Content/EffectsForgeSwitch/Outline.vert
ddc88d0bad64e068\tfrag\t\t/Content/EffectsForgeSwitch/DefaultBlank.frag
";

fn res(kind: Kind, set: u32, binding: u32, slot: u32) -> Resource {
    Resource {
        kind,
        set,
        binding,
        slot,
        count: 1,
    }
}

#[test]
fn reads_the_reference_rows() {
    let rows = parse(REFERENCE).unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(key_hex(&rows[0].key), "080e48b1582b0020");
    assert_eq!(rows[0].stage, Stage::Frag);
    assert_eq!(
        rows[0].resources,
        vec![
            res(Kind::Sampler, 0, 24, 0),
            res(Kind::Uniform, 1, 37, 0),
            res(Kind::Texture, 2, 0, 0)
        ]
    );
    assert_eq!(rows[0].source, "/Content/EffectsForgeSwitch/Shroud2.frag");
    assert_eq!(rows[1].stage, Stage::Vert);
    assert_eq!(rows[2].resources.len(), 8);
    assert_eq!(rows[2].resources[3], res(Kind::Texture, 1, 31, 3));
    assert_eq!(rows[3].resources[1], res(Kind::Uniform, 1, 47, 1));
    // a shader without resources keeps its empty field
    assert!(rows[4].resources.is_empty());
}

#[test]
fn rows_round_trip() {
    let rows = parse(REFERENCE).unwrap();
    let text = write(&rows, "nxb-spirv 1, naga 30.0.1");
    assert!(text.starts_with(&format!("# {HEAD}\n# nxb-spirv 1, naga 30.0.1\n")));
    assert_eq!(parse(&text).unwrap(), rows);
    // the rows are the reference's text
    let body = |t: &str| -> Vec<String> {
        t.lines()
            .filter(|l| !l.starts_with('#'))
            .map(String::from)
            .collect()
    };
    assert_eq!(body(&text), body(REFERENCE));
}

#[test]
fn every_kind_stage_and_count_round_trips() {
    let kinds = [
        Kind::Uniform,
        Kind::Storage,
        Kind::Texture,
        Kind::Sampler,
        Kind::Combined,
    ];
    let mut resources: Vec<Resource> = kinds
        .iter()
        .enumerate()
        .map(|(k, &kind)| Resource {
            kind,
            set: k as u32,
            binding: 100 + k as u32,
            slot: 2,
            count: 1 + k as u32,
        })
        .collect();
    resources.push(res(Kind::Push, 0, 0, 30));
    let rows = vec![
        Row {
            key: [0xff; 8],
            stage: Stage::Comp([8, 4, 1]),
            resources,
            source: "/a b/c.spv".into(),
        },
        Row {
            key: [0; 8],
            stage: Stage::Vert,
            resources: Vec::new(),
            source: String::new(),
        },
    ];
    let text = write(&rows, "");
    assert!(text.contains("\tcomp:8,4,1\tu:0.100=2 b:1.101=2*2 t:2.102=2*3 "));
    assert!(text.contains("c:4.104=2*5 p:0.0=30\t"));
    let back = parse(&text).unwrap();
    assert_eq!(back[0], rows[1]);
    assert_eq!(back[1], rows[0]);
}

#[test]
fn write_sorts_and_keeps_the_first_row_of_a_key() {
    let mk = |key: u8, source: &str| Row {
        key: [key; 8],
        stage: Stage::Frag,
        resources: Vec::new(),
        source: source.into(),
    };
    let text = write(&[mk(2, "/b"), mk(1, "/a"), mk(2, "/dup\tx")], "one\ntwo");
    let want = format!(
        "# {HEAD}\n# one\n# two\n0101010101010101\tfrag\t\t/a\n0202020202020202\tfrag\t\t/b\n"
    );
    assert_eq!(text, want);
    // a tab in a source does not make a fifth field
    assert_eq!(
        write(&[mk(3, "a\tb")], "")
            .lines()
            .last()
            .unwrap()
            .split('\t')
            .count(),
        4
    );
}

#[test]
fn keys_and_function_names() {
    let key = parse_key("0FCCA318097ae0a2").unwrap();
    assert_eq!(key, [0x0f, 0xcc, 0xa3, 0x18, 0x09, 0x7a, 0xe0, 0xa2]);
    assert_eq!(key_hex(&key), "0fcca318097ae0a2");
    assert_eq!(function(&key), "vk_0fcca318097ae0a2");
    assert_eq!(dual_function(&key), "vk_0fcca318097ae0a2_d");
    for bad in [
        "",
        "0fcca318097ae0a",
        "0fcca318097ae0a2f",
        "0fcca318097ae0g2",
        "+fcca318097ae0a2",
    ] {
        assert_eq!(parse_key(bad), None, "{bad}");
    }
}

#[test]
fn reads_crlf_and_blank_lines() {
    let rows = parse("# c\r\n\r\n0000000000000001\tvert\tu:0.0=0\t/v\r\n\n").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "/v");
}

#[test]
fn malformed_lines_are_refused() {
    let ok = "0000000000000001\tvert\tu:0.0=0\t/v";
    assert!(parse(ok).is_ok());
    let bad = [
        // fields
        "0000000000000001\tvert\tu:0.0=0",
        "0000000000000001\tvert\tu:0.0=0\t/v\textra",
        "0000000000000001 vert u:0.0=0 /v",
        // key
        "000000000000001\tvert\t\t/v",
        "000000000000000z\tvert\t\t/v",
        // stage
        "0000000000000001\tgeom\t\t/v",
        "0000000000000001\tcomp:1,1\t\t/v",
        "0000000000000001\tcomp:1,0,1\t\t/v",
        "0000000000000001\tcomp:1,1,1,1\t\t/v",
        "0000000000000001\tcomp:+1,1,1\t\t/v",
        // resources
        "0000000000000001\tvert\tx:0.0=0\t/v",
        "0000000000000001\tvert\tu0.0=0\t/v",
        "0000000000000001\tvert\tu:0,0=0\t/v",
        "0000000000000001\tvert\tu:0.0\t/v",
        "0000000000000001\tvert\tu:0.0=-1\t/v",
        "0000000000000001\tvert\tu:0.0=0*\t/v",
        "0000000000000001\tvert\tu:0.0=0*0\t/v",
        "0000000000000001\tvert\tu:0.0=0@3\t/v",
        "0000000000000001\tvert\tu:8.0=0\t/v",
        "0000000000000001\tvert\tu:0.128=0\t/v",
        "0000000000000001\tvert\tu:0.0=31\t/v",
        "0000000000000001\tvert\tt:0.0=30*2\t/v",
        "0000000000000001\tvert\ts:0.0=16\t/v",
        "0000000000000001\tvert\tc:0.0=15*2\t/v",
        "0000000000000001\tvert\tp:0.1=0\t/v",
        "0000000000000001\tvert\tp:0.0=0*2\t/v",
        "0000000000000001\tvert\tu:0.0=99999999999\t/v",
        // the old engine's argument-buffer and dynamic-offset kinds are not part of this table
        "0000000000000001\tvert\tU:1.0=3@0\t/v",
        "0000000000000001\tvert\tq:0.0=4\t/v",
    ];
    for b in bad {
        let e = parse(&format!("# head\n{b}\n")).unwrap_err();
        assert_eq!(e.line, 2, "{b}");
        assert!(e.to_string().starts_with("vk_shaders.tsv line 2: "), "{e}");
    }
}

#[test]
fn a_key_in_two_rows_is_refused() {
    let t = "0000000000000001\tvert\t\t/a\n0000000000000002\tfrag\t\t/b\n0000000000000001\tfrag\t\t/c\n";
    let e = parse(t).unwrap_err();
    assert_eq!(e.line, 3);
    assert!(e.what.contains("0000000000000001"), "{e}");
}

#[test]
fn kinds_and_letters_agree() {
    for c in "ubtscp".chars() {
        assert_eq!(Kind::from_letter(c).unwrap().letter(), c);
    }
    assert_eq!(Kind::from_letter('U'), None);
    assert!(Kind::Combined.is_texture() && Kind::Combined.is_sampler());
    assert!(Kind::Push.is_buffer() && !Kind::Push.is_texture());
    assert_eq!(Kind::Combined.slots(), SAMPLER_SLOTS);
}
