//! Raw global memory lookups that start from the entry they chose last (`GlobalToStorage.TryGenerateRawStorageOp`).
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

fn msl() -> String {
    let job = parse_jobs(
        r#"[{"key": "r", "stage": "cs", "arch": "sm86", "local": [1, 1, 1], "shared": 4096}]"#,
    )
    .expect("job")
    .remove(0);
    // Two loads, the second 4 KiB on (too far for one group, see raw_group_tests), each kept by a store to shared
    // memory
    let ops = [
        s2r_tid(9),
        s2r_tid(5),
        LDG,
        STORE,
        (LDG.0 | 4096 << 40, LDG.1),
        STORE,
    ];
    let mut code = Vec::new();
    for (lo, hi) in ops.into_iter().map(waiting).chain([AEXIT]) {
        code.extend(lo.to_le_bytes());
        code.extend(hi.to_le_bytes());
    }
    let t = translate(&job, &code, &Context::default()).expect("translate");
    t.libraries[0].msl.clone()
}

#[test]
fn two_loads_share_one_zeroed_word() {
    let m = msl();
    let main = &m[m.find("kernel void main0").expect("entry")..];
    let decls: Vec<&str> = main
        .lines()
        .filter(|l| l.starts_with("    uint lm") && l.ends_with(" = {};"))
        .collect();
    assert_eq!(decls.len(), 1, "{m}");
    let word = decls[0]
        .trim_start_matches("    uint ")
        .trim_end_matches(" = {};");
    // The helper takes the word, reads it before the chunk's hint and records the hint's or the search's choice
    let head = format!("thread uint& {word}");
    let at = m.find(&head).expect("the helper takes the word");
    let body = &m[at..];
    let body = &body[..body.find("\n}\n").expect("helper end")];
    let first = body.find(&format!("({word})")).expect("the word is read");
    let hint = body
        .find(">> 21u")
        .or_else(|| body.find(">> 21"))
        .expect("the hint");
    assert!(first < hint, "{body}");
    assert!(body.contains(&format!("    {word} = ")), "{body}");
}
