//! Groups of raw global word loads: one resolve of the range, then loads at the host address (`GlobalToStorage`).
#![cfg(test)]

use super::{Context, translate};
use crate::job::parse_jobs;
use crate::translation::emit_test_util::AEXIT;

/// `S2R Rd, SR_TID.X`.
fn s2r_tid(rd: u64) -> (u64, u64) {
    (0x7919 | rd << 16, 0x000e_2600_0000_2100)
}

/// The instruction waits on every scoreboard (control bits 116..121).
fn waiting((lo, hi): (u64, u64)) -> (u64, u64) {
    (lo, hi | 0x3f << 52)
}

/// `LDG.E R6, [R5.64 + UR4]`: R5 the thread index, an address the translator traces to no storage buffer.
const LDG: (u64, u64) = (0x0000_0004_0506_7981, 0x000e_a200_081e_890f);
/// `STS.U32 [R9 * 16 + 8], R6`: the sink, R9 the thread index.
const STORE: (u64, u64) = (0x0000_0806_0900_8388, 0x000f_e800_0000_c800);

/// `LDG` of the word `offset` bytes past the base.
fn ldg(offset: u64) -> (u64, u64) {
    (LDG.0 | offset << 40, LDG.1)
}

/// The MSL of a kernel that loads the words at these offsets from one base, each kept by a store to shared memory.
fn msl(offsets: &[u64]) -> String {
    let job = parse_jobs(
        r#"[{"key": "r", "stage": "cs", "arch": "sm86", "local": [1, 1, 1], "shared": 4096}]"#,
    )
    .expect("job")
    .remove(0);
    let mut ops = vec![s2r_tid(9), s2r_tid(5)];
    for o in offsets {
        ops.push(ldg(*o));
        ops.push(STORE);
    }
    let mut code = Vec::new();
    for (lo, hi) in ops.into_iter().map(waiting).chain([AEXIT]) {
        code.extend(lo.to_le_bytes());
        code.extend(hi.to_le_bytes());
    }
    let t = translate(&job, &code, &Context::default()).expect("translate");
    t.libraries[0].msl.clone()
}

/// The helpers of a library: name, number of word arguments and body.
fn helpers(m: &str) -> Vec<(String, usize, String)> {
    let mut v = Vec::new();
    let mut rest = m;
    while let Some(at) = rest.find("\nint fn") {
        let def = &rest[at + 1..];
        let name: String = def[4..]
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect();
        let head = &def[..def.find(')').expect("parameters")];
        let args = head.matches("thread int& a").count();
        let body = &def[..def.find("\n}\n").expect("end")];
        v.push((name, args, body.to_string()));
        rest = &def[body.len()..];
    }
    v
}

/// The number of calls of the helper in the kernel.
fn calls(m: &str, name: &str) -> usize {
    m.matches(&format!(" = {name}(")).count()
}

/// The helper with this many word arguments.
fn with_args(m: &str, args: usize) -> Option<(String, String)> {
    helpers(m)
        .into_iter()
        .find(|h| h.1 == args)
        .map(|h| (h.0, h.2))
}

#[test]
fn loads_near_one_base_share_one_resolve() {
    let m = msl(&[0, 4, 8]);
    let (range, range_body) = with_args(&m, 4).expect("the range helper");
    let (load, load_body) = with_args(&m, 5).expect("the load helper");
    assert_eq!(calls(&m, &range), 1, "{m}");
    assert_eq!(calls(&m, &load), 3, "{m}");
    // No access searches the table by itself: the helper of one word accesses (two arguments) is not called
    assert!(
        with_args(&m, 2).is_none_or(|(n, _)| calls(&m, &n) == 0),
        "{m}"
    );
    // The resolve takes the first byte and the span of the range: 0 and 12 bytes
    let call = m
        .lines()
        .find(|l| l.contains(&format!(" = {range}(")))
        .expect("the call");
    assert!(call.contains(&format!("{range}(")), "{call}");
    assert!(m.contains(" = 12;"), "the span of three words\n{m}");
    // The resolve checks the end of the range against the entry's end
    assert!(range_body.contains("+ (-1)"), "{range_body}");
    // The resolve and the load helper each keep the entry they chose last in a word of their own
    let words = m
        .lines()
        .filter(|l| l.starts_with("    uint lm") && l.ends_with(" = {};"))
        .count();
    assert_eq!(words, 2, "{m}");
    // The loads read a device pointer made of the address and the entry's delta
    assert!(
        load_body.contains("reinterpret_cast<device uint*>"),
        "{load_body}"
    );
}

#[test]
fn a_range_no_entry_holds_falls_back_to_the_search_per_word() {
    let m = msl(&[0, 4, 8]);
    let (range, range_body) = with_args(&m, 4).expect("the range helper");
    let (_, load_body) = with_args(&m, 5).expect("the load helper");
    // The resolve answers 0 (no entry) unless the first and the last byte are both below the entry's end
    assert!(
        range_body.contains("? ") && range_body.contains(": 0)"),
        "{range_body}"
    );
    assert!(calls(&m, &range) == 1);
    // The load helper takes the resolved branch only for a chosen entry, and searches the table for the others
    let early = load_body.find("(a2 != 0)").expect("the entry test");
    let hint = load_body.find(">> 21u").expect("the hint of the fallback");
    let search = load_body
        .find("+ 4096")
        .expect("the search of the fallback");
    assert!(early < hint && hint < search, "{load_body}");
    // The fast branch returns before the fallback starts
    let ret = load_body[early..].find("return ").expect("early return") + early;
    assert!(ret < hint, "{load_body}");
}

#[test]
fn a_single_load_searches_the_table_as_before() {
    let m = msl(&[0]);
    assert!(with_args(&m, 4).is_none(), "{m}");
    assert!(with_args(&m, 5).is_none(), "{m}");
    let (name, _) = with_args(&m, 2).expect("the helper of one access");
    assert_eq!(calls(&m, &name), 1, "{m}");
}

#[test]
fn two_far_loads_are_not_one_group() {
    // 4 KiB apart: more than a group spans
    let m = msl(&[0, 4096]);
    assert!(with_args(&m, 4).is_none(), "{m}");
}
