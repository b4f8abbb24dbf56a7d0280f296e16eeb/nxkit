//! A compute shader with a `CAL`: no reference output exists for it (no corpus shader calls a function), so this checks
//! the shape of the structured program: the callee is translated with its input and output registers as arguments and called.

use super::msl_entry::structure_job;
use crate::job::parse_jobs;

fn words(prog: &[u64]) -> Vec<u8> {
    prog.iter().flat_map(|w| w.to_le_bytes()).collect()
}

const PRED7: u64 = 0x7 << 16;

fn mov32i(rd: u64, imm: u32) -> u64 {
    0x40u64 << 50 | rd | (imm as u64) << 20 | PRED7 | 0xf << 12
}

fn mov(rd: u64, rs: u64) -> u64 {
    0x1726u64 << 50 | rd | rs << 20 | PRED7 | 0xf << 39
}

fn cal(from: u64, to: u64) -> u64 {
    0x3898u64 << 50 | ((to - from - 8) & 0xffffff) << 20 | PRED7
}

const RET: u64 = 0x38c8u64 << 50 | 0x70000 | 0xf;

#[test]
fn called_function_is_translated_with_arguments() {
    let jobs = parse_jobs(r#"[{"key": "t", "stage": "cs", "local": [1, 1, 1]}]"#).expect("job");
    // 0: scheduling word, 8: MOV32I R1, 16: CAL 40 (the decoder ends the block: nothing after an unconditional CAL is
    // decoded), 32: scheduling word, 40: MOV R2, R1, 48: RET
    let code = words(&[
        0,
        mov32i(1, 0x3f80_0000),
        cal(16, 40),
        0,
        0,
        mov(2, 1),
        RET,
        0,
    ]);
    let s = structure_job(&jobs[0], &code).expect("structure");
    // fun0 is the entry point, fun1 the callee with the input register R1 and the output register R2 as arguments
    assert_eq!(s.info.functions.len(), 2);
    assert_eq!(s.info.functions[0].in_arguments.len(), 0);
    assert_eq!(s.info.functions[1].in_arguments.len(), 1);
    assert_eq!(s.info.functions[1].out_arguments.len(), 1);
}
