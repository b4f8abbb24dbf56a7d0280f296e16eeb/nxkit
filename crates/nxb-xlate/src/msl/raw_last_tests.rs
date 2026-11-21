//! Raw global memory lookups that start from the entry they chose last, whose range and delta the invocation keeps
//! (`GlobalToStorage.TryGenerateRawStorageOp`).
#![cfg(test)]

use super::{Context, translate};
use crate::front::emit_test_util::AEXIT;
use crate::job::parse_jobs;

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

/// The kernel's zeroed last-entry arrays (`uint lmN[7] = {};`) by name.
fn last_arrays(m: &str) -> Vec<String> {
    let main = &m[m.find("kernel void main0").expect("entry")..];
    main.lines()
        .filter(|l| l.starts_with("    uint lm") && l.ends_with("[7] = {};"))
        .map(|l| {
            l.trim_start_matches("    uint ")
                .trim_end_matches("[7] = {};")
                .to_string()
        })
        .collect()
}

/// The body of the helper that takes the array `word`.
fn helper<'a>(m: &'a str, word: &str) -> &'a str {
    let head = format!("thread uint (&{word})[7]");
    let at = m.find(&head).expect("the helper takes the array");
    let body = &m[at..];
    &body[..body.find("\n}\n").expect("helper end")]
}

#[test]
fn two_loads_share_one_zeroed_array() {
    let m = msl();
    let words = last_arrays(&m);
    assert_eq!(words.len(), 1, "{m}");
    let word = &words[0];
    let body = helper(&m, word);
    // The helper checks the range it copied last before the chunk's hint: the start and the end words
    let hint = body
        .find(">> 21u")
        .or_else(|| body.find(">> 21"))
        .expect("the hint");
    for w in 1..=4 {
        let at = body
            .find(&format!("({word}[{w}])"))
            .unwrap_or_else(|| panic!("word {w} is read\n{body}"));
        assert!(at < hint, "{body}");
    }
    // A miss records the index and copies the entry's six words
    for w in 0..=6 {
        assert!(body.contains(&format!("    {word}[{w}] = ")), "{body}");
    }
}

#[test]
fn a_hit_reads_the_delta_from_the_copy() {
    let m = msl();
    let word = &last_arrays(&m)[0];
    let body = helper(&m, word);
    // The host address adds the copied delta: after the last copy into the array no table word is read
    let filled = body.rfind(&format!("    {word}[6] = ")).expect("the fill");
    let rest = &body[filled..];
    assert!(rest.contains(&format!("({word}[5])")), "{rest}");
    assert!(rest.contains(&format!("({word}[6])")), "{rest}");
    assert!(!rest.contains("._m0["), "{rest}");
}
