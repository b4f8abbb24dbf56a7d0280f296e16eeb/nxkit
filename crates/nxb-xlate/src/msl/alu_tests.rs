//! Ampere ALU results stored to shared memory so they are kept.
#![cfg(test)]

use super::tensor_tests::msl;

/// `STS.U32 [RZ], R6`.
const STS_R6: (u64, u64) = (0x0000_0006_ff00_7388, 0x000f_e800_0000_0800);
/// `SEL R6, RZ, 0x1, !P0`: R6 = P0 ? 1 : 0.
const SEL_P0: (u64, u64) = (0x0000_0001_ff06_7807, 0x000f_c200_0400_00ff);

/// The MSL of `ops` with R6 stored.
fn kept(ops: &[(u64, u64)]) -> String {
    let mut all = ops.to_vec();
    all.push(STS_R6);
    msl(&all)
}

/// The value the last store writes.
fn stored(ops: &[(u64, u64)]) -> String {
    let m = kept(ops);
    let main = &m[m.find("kernel void main0").expect("entry")..];
    let line = main
        .lines()
        .rfind(|l| l.contains("sm0[") && l.contains("] = "))
        .unwrap_or_else(|| panic!("no store\n{m}"));
    line[line.find("] = ").expect("store") + 4..]
        .trim_end_matches(';')
        .to_string()
}

#[test]
fn popc_immediate_top_bit_is_no_not() {
    // POPC R6, 0x800000ff
    assert_eq!(
        stored(&[(0x8000_00ff_ff06_7909, 0x000f_c200_0000_0000)]),
        "9u"
    );
}

#[test]
fn flo_immediate_top_bit_is_no_not() {
    // FLO.U32 R6, 0x800400ff: 0x800400ff itself, not its bitwise not
    let m = kept(&[(0x8004_00ff_ff06_7900, 0x000f_c200_000e_0000)]);
    assert!(m.contains("dm_find_umsb(2147746047u)"), "{m}");
}

#[test]
fn iadd3_carry_of_two_sets_both_predicates() {
    // IADD3 R7, P0, P1, -RZ, -0x1, -RZ: 2^32 + 0xffffffff + 2^32
    let iadd3 = (0xffff_ffff_ff07_7810, 0x000f_c200_0791_e9ff);
    assert_eq!(stored(&[iadd3, SEL_P0]), "1u");
}

#[test]
fn plop3_second_table_reads_the_old_source() {
    // ISETP.EQ.U32.AND P2, PT, RZ, RZ, PT: P2 = true
    let isetp = (0x0000_00ff_ff00_720c, 0x000f_c200_03f4_2070);
    // PLOP3.LUT P2, P0, P2, PT, PT, 0x0, 0xf0: P2 = false, P0 = the old P2
    let plop3 = (0x0000_0000_00f0_781c, 0x000f_c200_0104_e070);
    assert_eq!(stored(&[isetp, plop3, SEL_P0]), "1u");
}

#[test]
fn lea_hi_sx32_sign_extends() {
    // MOV R5, 0x80000000, then LEA.HI.SX32 R6, R5, RZ, 0x1: (0xffffffff:R5) >> 31
    let mov = (0x8000_0000_ff05_7802, 0x000f_c200_0000_0fff);
    let lea = (0x0000_00ff_0506_7211, 0x000f_c200_078f_0aff);
    assert_eq!(stored(&[mov, lea]), "4294967295u");
}

#[test]
fn lea_negated_zero_b_carries() {
    // LEA R6, P0, RZ, -RZ, 0x0: ~0 + 1 carries out
    let lea = (0x8000_00ff_ff06_7211, 0x000f_c200_0780_00ff);
    assert_eq!(stored(&[lea, SEL_P0]), "1u");
}

#[test]
fn lea_x_not_b() {
    // LEA.X R6, RZ, ~RZ, 0x0, PT: 0 + 0xffffffff + 1
    let lea = (0x8000_00ff_ff06_7211, 0x000f_c200_038e_04ff);
    assert_eq!(stored(&[lea]), "0u");
}

/// `S2R Rr, SR_TID.X`: a register the translator cannot fold.
fn s2r(r: u64) -> (u64, u64) {
    (0x0000_0000_0000_7919 | r << 16, 0x000e_2600_0000_2100)
}

/// The MSL of `ops` after `S2R` of R9 and of each register in `srcs`, with R6 stored.
fn kept_srcs(ops: &[(u64, u64)], srcs: &[u64]) -> String {
    let mut all: Vec<_> = [9].iter().chain(srcs).map(|&r| s2r(r)).collect();
    all.extend_from_slice(ops);
    kept(&all)
}

/// The lanes that the unpacks of half pairs read, `0` or `1`.
fn lanes(m: &str) -> Vec<char> {
    let mut out: Vec<char> = m
        .match_indices("as_type<half2>(")
        .filter_map(|(at, _)| {
            let tail = &m[at..];
            let end = tail.find(")[")?;
            tail[end + 2..].chars().next()
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// `HMUL2 R6, R19.H0_H0, c[0x0][0x168].H0_H0`.
const HMUL2_C_H0: (u64, u64) = (0x2000_5a00_1306_7a32, 0x000f_e200_0000_0800);
/// `HMUL2 R6, R19.H1_H1, c[0x0][0x168].H1_H1`.
const HMUL2_C_H1: (u64, u64) = (0x3000_5a00_1306_7a32, 0x000f_e200_0000_0c00);
/// `HMUL2 R6, R19.H0_H0, R20.F32`.
const HMUL2_F32: (u64, u64) = (0x1000_0014_1306_7232, 0x000f_e200_0000_0800);

#[test]
fn half_constant_low_half_in_both_lanes() {
    let m = kept_srcs(&[HMUL2_C_H0], &[19]);
    assert_eq!(lanes(&m), ['0'], "{m}");
}

#[test]
fn half_constant_high_half_in_both_lanes() {
    let m = kept_srcs(&[HMUL2_C_H1], &[19]);
    assert_eq!(lanes(&m), ['1'], "{m}");
}

#[test]
fn half_f32_operand_is_the_float() {
    let m = kept_srcs(&[HMUL2_F32], &[19, 20]);
    // A's unpack alone: R20 is used as a float, not split into halves
    assert_eq!(m.matches("as_type<half2>(").count(), 1, "{m}");
    assert_eq!(lanes(&m), ['0'], "{m}");
}

/// `HSETP2.GT.FTZ.AND P0, PT, R19, R20, PT`.
const HSETP2_FTZ: (u64, u64) = (0x0000_0014_1300_7234, 0x000f_ca00_03f1_4000);
/// `HSETP2.GT.AND P0, PT, R19, R20, PT`.
const HSETP2: (u64, u64) = (0x0000_0014_1300_7234, 0x000f_ca00_03f0_4000);
/// `HSET2.BF.GT.FTZ.AND R6, R19, R20, PT`.
const HSET2_FTZ: (u64, u64) = (0x0000_0014_1306_7233, 0x000f_e200_0381_4080);
/// `FSEL R6, R19, R20, P0`: reads P0.
const FSEL_P0: (u64, u64) = (0x0000_0014_1306_7208, 0x000f_e400_0000_0000);
/// The half's own subnormal bound, 2^-14.
const BOUND: &str = "6.1035156e-5f";

#[test]
fn hsetp2_ftz_flushes_both_operands() {
    // Only P0 (the low lanes) is read. The high lanes' comparison goes to PT and is dropped with its flushes.
    let m = kept_srcs(&[HSETP2_FTZ, FSEL_P0], &[19, 20]);
    assert_eq!(m.matches(BOUND).count(), 2, "{m}");
}

#[test]
fn hset2_ftz_flushes_both_lanes_of_both_operands() {
    let m = kept_srcs(&[HSET2_FTZ], &[19, 20]);
    assert_eq!(m.matches(BOUND).count(), 4, "{m}");
}

#[test]
fn hsetp2_without_ftz_compares_as_is() {
    let m = kept_srcs(&[HSETP2, FSEL_P0], &[19, 20]);
    assert!(!m.contains(BOUND), "{m}");
}

/// `I2F.U32.RP R6, R19`.
const I2F_RP: (u64, u64) = (0x0000_0013_0006_7306, 0x000f_e200_0020_9000);
/// `I2F.U32 R6, R19`.
const I2F_RN: (u64, u64) = (0x0000_0013_0006_7306, 0x000f_e200_0020_1000);
/// `F2F.F16.F32.RP R6, R19`.
const F2F_RP: (u64, u64) = (0x0000_0013_0006_7304, 0x000f_e200_0020_8800);
/// `F2F.F16.F32 R6, R19`.
const F2F_RN: (u64, u64) = (0x0000_0013_0006_7304, 0x000f_e200_0020_0800);
/// 2^32 as the MSL prints the float: the bound above which the rounded float is above every u32.
const U32_END: &str = "4294967300.0f";

#[test]
fn i2f_rp_checks_against_the_integer() {
    let m = kept_srcs(&[I2F_RP], &[19]);
    assert!(m.contains(U32_END), "{m}");
    // The float's bits stepped up
    assert!(m.contains(") + 1)"), "{m}");
}

#[test]
fn i2f_to_nearest_is_plain() {
    let m = kept_srcs(&[I2F_RN], &[19]);
    assert!(!m.contains(U32_END), "{m}");
}

#[test]
fn f2f_rp_reads_the_half_back() {
    // Through a mask: an unpack of the pack would be folded into the float itself
    let m = kept_srcs(&[F2F_RP], &[19]);
    assert!(m.contains("& 65535"), "{m}");
}

#[test]
fn f2f_to_nearest_is_plain() {
    let m = kept_srcs(&[F2F_RN], &[19]);
    assert!(!m.contains("& 65535"), "{m}");
}

/// `MUFU.COS R6, R19`.
const MUFU_COS: (u64, u64) = (0x0000_0013_0006_7308, 0x000f_e600_0000_0000);
/// `MUFU.SIN R6, R19`.
const MUFU_SIN: (u64, u64) = (0x0000_0013_0006_7308, 0x000f_e600_0000_0400);

/// `f` of the turns through their fraction: `r = rint(t)`, `d = (t - r)`, `a = (d * 2pi)`, then `f(a)`.
fn lowered(op: (u64, u64), f: &str) {
    let m = kept_srcs(&[op], &[19]);
    let lhs = |l: &str| l.trim().split(" = ").next().unwrap_or("").to_string();
    let lines: Vec<&str> = m.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(" = rint("))
        .unwrap_or_else(|| panic!("no rint\n{m}"));
    let r = lhs(lines[at]);
    let t = &lines[at][lines[at].find("rint(").expect("rint") + 5..lines[at].len() - 2];
    let d = lhs(lines[at + 1]);
    let a = lhs(lines[at + 2]);
    assert_eq!(lines[at + 1].trim(), format!("{d} = ({t} - {r});"), "{m}");
    assert_eq!(
        lines[at + 2].trim(),
        format!("{a} = ({d} * 6.2831855f);"),
        "{m}"
    );
    assert!(
        lines[at + 3].trim().ends_with(&format!("= {f}({a});")),
        "{m}"
    );
}

#[test]
fn mufu_cos_takes_the_fraction_of_the_turns() {
    lowered(MUFU_COS, "cos");
}

#[test]
fn mufu_sin_takes_the_fraction_of_the_turns() {
    lowered(MUFU_SIN, "sin");
}
