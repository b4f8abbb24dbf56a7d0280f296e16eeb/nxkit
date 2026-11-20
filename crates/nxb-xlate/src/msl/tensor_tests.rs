//! The tensor pass's HMMA chains (`TensorPass.Prepare`): the flags two chained HMMAs on an LDSM's matrices get.
#![cfg(test)]

use super::{Context, translate};
use crate::front::emit_test_util::AEXIT;
use crate::job::parse_jobs;

/// The instruction waits on every scoreboard (control bits 116..121).
fn waiting((lo, hi): (u64, u64)) -> (u64, u64) {
    (lo, hi | 0x3f << 52)
}

/// `S2R R9, SR_TID.X`.
const S2R: (u64, u64) = (0x0000_0000_0009_7919, 0x000e_2600_0000_2100);
/// `IMAD R2, R9, 16, RZ`.
const IMAD: (u64, u64) = (0x0000_0010_0902_7824, 0x000f_e200_078e_02ff);
/// `LDSM.16.M88.4 R36, [R2]`.
const LDSM: (u64, u64) = (0x0000_0000_0224_783b, 0x000e_2200_0000_0200);
/// `HMMA.16816.F16 R46, R36, R4, RZ`.
const HMMA0: (u64, u64) = (0x0000_0004_242e_723c, 0x041f_f000_0000_08ff);
/// `HMMA.16816.F16 R46, R36, R6, R46`.
const HMMA1: (u64, u64) = (0x0000_0006_242e_723c, 0x000f_e200_0000_082e);
/// `STS.U32 [R9 * 16 + 8], R46`.
const STS0: (u64, u64) = (0x0000_082e_0900_8388, 0x000f_e800_0000_c800);
/// `STS.U32 [R9 * 16 + 12], R47`: D's two words, as a network stores them.
const STS1: (u64, u64) = (0x0000_0c2f_0900_8388, 0x000f_e800_0000_c800);

pub(super) fn msl(ops: &[(u64, u64)]) -> String {
    let job = parse_jobs(
        r#"[{"key": "k", "stage": "cs", "arch": "sm86", "local": [32, 1, 1], "shared": 4096}]"#,
    )
    .expect("job")
    .remove(0);
    let mut code = Vec::new();
    for &(lo, hi) in ops
        .iter()
        .map(|o| waiting(*o))
        .collect::<Vec<_>>()
        .iter()
        .chain([&AEXIT])
    {
        code.extend(lo.to_le_bytes());
        code.extend(hi.to_le_bytes());
    }
    let t = translate(&job, &code, &Context::default()).expect("translate");
    t.libraries[0].msl.clone()
}

/// The arguments of each helper call in the entry point.
fn calls(m: &str) -> Vec<Vec<String>> {
    let main = &m[m.find("kernel void main0").expect("entry")..];
    let mut out = Vec::new();
    let mut rest = main;
    while let Some(at) = rest.find("fn1(") {
        let tail = &rest[at + 4..];
        let mut depth = 1;
        let end = tail
            .char_indices()
            .find(|&(_, c)| {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                depth == 0
            })
            .expect("call end")
            .0;
        let mut args = Vec::new();
        let (mut d, mut start) = (0, 0);
        for (i, c) in tail[..end].char_indices() {
            match c {
                '(' => d += 1,
                ')' => d -= 1,
                ',' if d == 0 => {
                    args.push(tail[start..i].trim().to_string());
                    start = i + 1;
                }
                _ => {}
            }
        }
        args.push(tail[start..end].trim().to_string());
        out.push(args);
        rest = &tail[end..];
    }
    out
}

/// The value of a call argument that is a constant, through the locals the call copies it from.
fn value(m: &str, a: &str) -> Option<i64> {
    let a = a
        .trim_start_matches("uint(")
        .trim_start_matches("int(")
        .trim_end_matches(')');
    if let Ok(v) = a.trim_end_matches('u').parse() {
        return Some(v);
    }
    // The assignment, not the declaration
    let pat = format!("{a} = ");
    let line = m.lines().map(str::trim).find(|l| l.starts_with(&pat))?;
    let rhs = line[pat.len()..].trim_end_matches(';');
    value(m, rhs)
}

/// The variable a call argument is a copy of.
fn root(m: &str, a: &str) -> String {
    let pat = format!("{a} = ");
    match m.lines().map(str::trim).find(|l| l.starts_with(&pat)) {
        Some(l) => {
            let rhs = l[pat.len()..].trim_end_matches(';');
            if rhs.starts_with('v') && rhs[1..].chars().all(|c| c.is_ascii_digit()) {
                root(m, rhs)
            } else {
                a.to_string()
            }
        }
        None => a.to_string(),
    }
}

#[test]
fn chained_hmmas_on_an_ldsm_keep_a_and_the_accumulator() {
    let m = msl(&[S2R, IMAD, LDSM, HMMA0, HMMA1, STS0, STS1]);
    let c = calls(&m);
    assert_eq!(c.len(), 2, "{m}");
    // 17 arguments and D1, then the matrices, then the shared memories
    assert_eq!(c[0][18], "dm_acc", "{m}");
    assert_eq!(c[0][19], "dm_am", "{m}");
    let v = |a: &str| value(&m, a).unwrap_or_else(|| panic!("{a} not constant\n{m}"));
    // C = 0, D to slot 0, A direct, and A's offsets and row stride
    assert_eq!(v(&c[0][10]), 8 | 4 | 16, "{m}");
    assert_eq!(
        [12, 13, 14, 15, 16].map(|n| v(&c[0][n])),
        [0, 128, 256, 384, 16],
        "{m}"
    );
    // A kept, C from slot 0, D to the registers
    assert_eq!(v(&c[1][10]), 1 | 2 | 16, "{m}");
    let addr = root(&m, &c[0][11]);
    assert_eq!(addr, root(&m, &c[1][11]), "the LDSM's row address\n{m}");
    assert!(m.contains(&format!("{addr} = (as_type<int>(")), "{m}");
    assert!(
        m.contains("    simdgroup_float8x8 dm_acc[16]; simdgroup_half8x8 dm_am[4];"),
        "{m}"
    );
}

/// `STS.U32 [R9 * 16 + 8], R36`: a store after the LDSM.
const STS_A: (u64, u64) = (0x0000_0824_0900_8388, 0x000f_e800_0000_c800);

/// The LDSM's reads lie between two SIMD-group barriers, and the store after it after the second.
#[test]
fn ldsm_reads_lie_between_subgroup_barriers() {
    let m = msl(&[S2R, IMAD, LDSM, STS_A]);
    let main = &m[m.find("kernel void main0").expect("entry")..];
    let at = |pat: &str| -> Vec<usize> { main.match_indices(pat).map(|(i, _)| i).collect() };
    let bars = at("simdgroup_barrier(mem_flags::mem_threadgroup);");
    assert_eq!(bars.len(), 2, "{m}");
    let reads = at("= as_type<float>(sm0[");
    let store = *at("sm0[")
        .iter()
        .find(|&&i| main[i..].split(';').next().unwrap().contains("] = "))
        .expect("store");
    assert!(!reads.is_empty(), "{m}");
    assert!(
        bars[0] < reads[0] && *reads.last().unwrap() < bars[1] && bars[1] < store,
        "{m}"
    );
}
