//! Helpers of the synthetic emit tests: Maxwell and Ampere programs built from words, translated up to the `emit` dump.
#![cfg(test)]

use super::pipeline::emit_text;
use crate::job::parse_jobs;

/// Predicate field of a Maxwell instruction: `@PT`.
pub const PRED7: u64 = 0x7 << 16;

/// The `emit` dump of a Maxwell compute program. `ops` are the instruction words, scheduling words are inserted.
pub fn maxwell_cs_text(ops: &[u64]) -> Result<String, String> {
    let jobs = parse_jobs(r#"[{"key": "t", "stage": "cs", "local": [1, 1, 1]}]"#)?;
    let mut words = vec![0u64];
    for (i, w) in ops.iter().enumerate() {
        words.push(*w);
        if i % 3 == 2 {
            words.push(0);
        }
    }
    words.extend([RET, 0]);
    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    emit_text(&jobs[0], &code)
}

/// `EXIT`.
pub const RET: u64 = 0xe300_0000_0000_0000 | 0x70000 | 0xf;

/// Guard of an Ampere instruction: `@PT`.
pub const APT: u64 = 0x7 << 12;

/// `EXIT` of Ampere.
pub const AEXIT: (u64, u64) = (0x94d | APT | 1 << 23, 0);

/// The `emit` dump of an Ampere compute program (`lo`, `hi` of every instruction), `EXIT` appended.
pub fn ampere_cs_text(ops: &[(u64, u64)]) -> Result<String, String> {
    ampere_text("cs", ops)
}

/// The same for a pixel shader (a header that only names the stage in front of the code).
pub fn ampere_ps_text(ops: &[(u64, u64)]) -> Result<String, String> {
    ampere_text("ps", ops)
}

/// The same for a pixel shader whose header has the colour target mask `targets` and writes depth.
pub fn ampere_ps_depth_text(targets: u32, ops: &[(u64, u64)]) -> Result<String, String> {
    let jobs = parse_jobs(r#"[{"key": "t", "stage": "ps", "local": [1, 1, 1], "arch": "sm86"}]"#)?;
    let mut header = [0u32; 32];
    header[0] = 5 << 10;
    header[18] = targets;
    header[19] = 2;
    let mut code: Vec<u8> = header.iter().flat_map(|w| w.to_le_bytes()).collect();
    for (lo, hi) in ops.iter().chain([AEXIT].iter()) {
        code.extend(lo.to_le_bytes());
        code.extend(hi.to_le_bytes());
    }
    emit_text(&jobs[0], &code)
}

/// The same for a vertex shader (a zero header in front of the code).
pub fn ampere_vs_text(ops: &[(u64, u64)]) -> Result<String, String> {
    ampere_text("vs", ops)
}

/// The same for a compute program of a job with the extra JSON members `extra` (for example `"nvn2_cs": [..]`).
pub fn ampere_cs_text_with(extra: &str, ops: &[(u64, u64)]) -> Result<String, String> {
    ampere_text_with("cs", extra, ops)
}

fn ampere_text(stage: &str, ops: &[(u64, u64)]) -> Result<String, String> {
    ampere_text_with(stage, "", ops)
}

fn ampere_text_with(stage: &str, extra: &str, ops: &[(u64, u64)]) -> Result<String, String> {
    let jobs = parse_jobs(&format!(
        r#"[{{"key": "t", "stage": "{stage}", "local": [1, 1, 1], "arch": "sm86"{extra}}}]"#
    ))?;
    let mut code = vec![0u8; if stage == "cs" { 0 } else { 0x80 }];
    if stage == "ps" {
        // header word 0: the shader type 5 in bits 10..14
        code[..4].copy_from_slice(&(5u32 << 10).to_le_bytes());
    }
    for (lo, hi) in ops.iter().chain([AEXIT].iter()) {
        code.extend(lo.to_le_bytes());
        code.extend(hi.to_le_bytes());
    }
    emit_text(&jobs[0], &code)
}

/// `LDC Rd, c[bank][offset]` with the address register `ra` and the address mode `mode` (bits 78..80), 32 bits wide.
pub fn ampere_ldc(rd: u64, ra: u64, bank: u64, offset: u64, mode: u64) -> (u64, u64) {
    let lo = 0xb82 | APT | rd << 16 | ra << 24 | (offset & 0xffff) << 38 | bank << 54;
    let hi = 4u64 << (73 - 64) | mode << (78 - 64);
    (lo, hi)
}
