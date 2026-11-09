//! Round trips and the rules of 2.11.

use crate::toml::{self, Value};
use crate::*;

const SAMPLE: &str = r#"
title_id = "0100000000010000"

[computed]
address_space = "64bit"   # NPDM
pool_size = 268_435_456
module_sizes = [1048576, 2097152]
mem_used = 3221225472
shader_groups = ["a", 'b']
pad_layout = "pro-controller"

[declared]
title = "Sample"
display_name = "Sample \"Game\""
op_mode = "docked"
present = "display"
fps_rules = ["menus-30"]

[declared.proof.op_mode]
capability = "op_mode"
test = "t_boot_docked"

[declared.proof.present]
capability = "present"
test = "t_present_pace"

[declared.proof.fps_rules]
capability = "frame pacing"
test = "t_fps"

[rules]
engine = "unreal"
frame_limit = { kind = "cvar", name = "t.MaxFPS" }
cuts = [
  { bind = "game", fingerprint = "ab12", bytes = "1f2003d5", why = "skips the splash" },
  { bind = "game", fingerprint = "cd34", bytes = "00", why = "no sleep" },
]
future_key = 7
"#;

#[test]
fn reads_all_three_sections() {
    let p = Profile::from_toml(SAMPLE).unwrap();
    assert_eq!(p.title_id.as_deref(), Some("0100000000010000"));
    assert_eq!(p.computed.pool_size, Some(268_435_456));
    assert_eq!(p.computed.module_sizes, [1048576, 2097152]);
    assert_eq!(p.declared.display_name.as_deref(), Some("Sample \"Game\""));
    assert_eq!(p.declared.present, Some(Present::Display));
    assert_eq!(
        p.rules.frame_limit,
        Some(FrameLimit {
            kind: "cvar".into(),
            name: "t.MaxFPS".into()
        })
    );
    assert_eq!(p.rules.cuts.len(), 2);
    assert_eq!(p.rules.extra.get("future_key"), Some(&Value::Int(7)));
    assert_eq!(
        p.get_str("rules.frame_limit.name").as_deref(),
        Some("t.MaxFPS")
    );
    assert_eq!(
        p.get_str("declared.proof.op_mode.test").as_deref(),
        Some("t_boot_docked")
    );
    assert_eq!(p.get("declared.nothing"), None);
    assert!(p.problems().is_empty(), "{:?}", p.problems());
}

#[test]
fn round_trip_is_stable() {
    let p = Profile::from_toml(SAMPLE).unwrap();
    let text = p.to_toml();
    let q = Profile::from_toml(&text).unwrap();
    assert_eq!(p, q);
    assert_eq!(q.to_toml(), text, "writing is deterministic");
    let empty = Profile::default();
    assert_eq!(empty.to_toml(), "");
    assert_eq!(Profile::from_toml("").unwrap(), empty);
    assert!(empty.is_provisional());
}

#[test]
fn unknown_sections_survive() {
    let p = Profile::from_toml("[computed]\nmem_used = 1\n\n[later]\nx = 1\n").unwrap();
    let again = Profile::from_toml(&p.to_toml()).unwrap();
    assert_eq!(p, again);
    assert!(p.to_toml().contains("[later]"));
}

#[test]
fn committed_text_has_no_computed_section() {
    let p = Profile::from_toml(SAMPLE).unwrap();
    let c = p.to_committed_toml();
    assert!(!c.contains("[computed]") && !c.contains("pool_size"));
    assert!(c.contains("[declared]") && c.contains("[rules]"));
}

#[test]
fn rules_of_the_design() {
    let p = Profile::from_toml("[declared]\nop_mode = \"docked\"\ndepth_mode = \"zero\"\n\n[rules]\ncuts = [{ bind = \"g\", fingerprint = \"f\", bytes = \"00\", why = \" \" }]\n").unwrap();
    let pr = p.problems();
    assert_eq!(pr.len(), 3, "{pr:?}");
    assert!(pr.iter().any(|m| m.contains("declared.op_mode needs")));
    assert!(
        pr.iter()
            .any(|m| m.contains("depth_mode is decided by a capability"))
    );
    assert!(pr.iter().any(|m| m.contains("rules.cuts[0] has no reason")));
}

#[test]
fn wrong_shapes_name_the_key() {
    let e = Profile::from_toml("[computed]\npool_size = \"big\"\n").unwrap_err();
    assert_eq!(
        e,
        ProfileError::Shape("computed.pool_size: a non-negative integer expected".into())
    );
    assert!(Profile::from_toml("[declared]\npresent = \"both\"\n").is_err());
    assert!(Profile::from_toml("[rules]\ncuts = [{ bind = \"g\" }]\n").is_err());
    assert!(matches!(
        Profile::from_toml("[a\n"),
        Err(ProfileError::Parse(_))
    ));
}

#[test]
fn toml_subset() {
    let t = toml::parse(
        "a = 1\nb.c = true\n[x.y]\nz = [1, 2,\n 3,]\ns = 'lit\\n'\nf = 1.5\nq = \"\\u00e9\\t\"\n",
    )
    .unwrap();
    assert_eq!(t["a"], Value::Int(1));
    let Value::Table(b) = &t["b"] else { return };
    assert_eq!(b["c"], Value::Bool(true));
    let Value::Table(x) = &t["x"] else { return };
    let Value::Table(y) = &x["y"] else { return };
    assert_eq!(
        y["z"],
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    assert_eq!(y["s"], Value::Str("lit\\n".into()));
    assert_eq!(y["f"], Value::Float(1.5));
    assert_eq!(y["q"], Value::Str("\u{e9}\t".into()));
    assert_eq!(toml::parse(&toml::write(&t)).unwrap(), t);
    for bad in [
        "a = 1\na = 2\n",
        "[t]\n[t]\n",
        "[[t]]\n",
        "a = \n",
        "a = \"x\n",
        "a = 1 2\n",
        "a = [1\n",
    ] {
        let r = toml::parse(bad);
        assert!(r.is_err(), "{bad:?} gave {r:?}");
        if let Err(e) = r {
            assert!(e.line >= 1);
        }
    }
}
