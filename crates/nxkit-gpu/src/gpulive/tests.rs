//! Tests of the live keys of the encoder.

use nxkit_abi::env::Env;

use super::settings::{FsTrace, GpuLive, LIVE, handle_list};

fn live(pairs: &[(&str, &str)]) -> GpuLive {
    GpuLive::from_env(&Env::from_pairs(pairs.iter().copied()))
}

#[test]
fn the_traces_are_off_without_their_keys() {
    let l = live(&[]);
    assert_eq!(l.bar_pass, None);
    assert_eq!(l.trace_fs, None);
    assert!(l.trace_handles.is_empty());
}

#[test]
fn barpass_is_a_pass_index() {
    assert_eq!(live(&[("NXKIT_BARPASS", " 12 ")]).bar_pass, Some(12));
    assert_eq!(live(&[("NXKIT_BARPASS", "x")]).bar_pass, None);
}

#[test]
fn a_fragment_trace_names_the_program_and_its_slots() {
    let all = FsTrace::parse("abc123").expect("a key");
    assert_eq!(all.program, "abc123");
    assert!(all.slots.is_empty() && all.wants(0) && all.wants(9));
    let some = FsTrace::parse(" abc123 : 1, 2,x,5").expect("a key");
    assert_eq!(some.slots, [1, 2, 5]);
    assert!(some.wants(2) && !some.wants(3));
    assert_eq!(FsTrace::parse(""), None);
    assert_eq!(FsTrace::parse(":1,2"), None);
    let l = live(&[("NXKIT_TRACE_FS", "k:3")]);
    assert_eq!(l.trace_fs.map(|t| t.slots), Some(vec![3]));
}

#[test]
fn handles_are_decimal_or_hex() {
    assert_eq!(
        handle_list("17, 0x10 ,zz,,0xfffff00000001"),
        [17, 16, 0xfffff00000001]
    );
    assert_eq!(
        live(&[("NXKIT_TRACE_HANDLES", "5,6")]).trace_handles,
        [5, 6]
    );
}

#[test]
fn every_new_key_is_a_live_key() {
    let names: Vec<&str> = LIVE.keys.iter().map(|k| k.name).collect();
    for k in ["NXKIT_BARPASS", "NXKIT_TRACE_FS", "NXKIT_TRACE_HANDLES"] {
        assert!(names.contains(&k), "{k}");
    }
}
