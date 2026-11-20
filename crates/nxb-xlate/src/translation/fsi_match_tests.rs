//! `FunctionMatch` on hand-encoded fragment shader interlock functions.
#![cfg(test)]

use super::fsi_match::{FunctionType, run_pass};
use crate::maxwell::attr::AttributeUsage;
use crate::maxwell::decoder::{Block, DecodedFunction, DecodedProgram};
use crate::maxwell::table::{InstOp, get_op};

const RZ: u64 = 255;
const PT: u64 = 7;

/// The opcode of an encoding row (`x` bits are zero) in the top bits of a word.
fn enc(row: &str) -> u64 {
    let bits: String = row
        .chars()
        .map(|c| if c == 'x' { '0' } else { c })
        .collect();
    u64::from_str_radix(&bits, 2).unwrap_or(0) << (64 - row.len())
}

/// `dest`, `a`, `b` are the usual register fields.
fn rd(w: u64, d: u64) -> u64 {
    w | d
}
fn ra(w: u64, a: u64) -> u64 {
    w | a << 8
}
fn rb(w: u64, b: u64) -> u64 {
    w | b << 20
}
fn guard(w: u64, pred: u64, inv: bool) -> u64 {
    w | pred << 16 | u64::from(inv) << 19
}
fn at(w: u64, bit: u32, v: u64) -> u64 {
    w | v << bit
}

fn s2r(d: u64, sr: u64) -> u64 {
    at(rd(guard(enc("1111000011001x"), PT, false), d), 20, sr)
}
fn lop32i(d: u64, a: u64, imm: u64) -> u64 {
    // And is logic op 0
    at(ra(rd(guard(enc("000001"), PT, false), d), a), 20, imm)
}
fn shr_w(d: u64, a: u64, imm: u64) -> u64 {
    let w = ra(rd(guard(enc("0011100x00101x"), PT, false), d), a);
    at(at(w, 20, imm), 39, 1)
}
fn iadd32i(d: u64, a: u64, imm: u64) -> u64 {
    at(ra(rd(guard(enc("0001110"), PT, false), d), a), 20, imm)
}
fn isetp(pd: u64, a: u64, b: u64, comp: u64) -> u64 {
    let w = ra(rb(guard(enc("010110110110xx"), PT, false), b), a);
    at(at(at(w, 3, pd), 49, comp), 39, PT) | 7
}
fn ret(pred: u64, inv: bool) -> u64 {
    guard(enc("111000110010xx"), pred, inv) | 15
}
fn ldg(d: u64, a: u64) -> u64 {
    // E, CG, B32
    let w = ra(rd(guard(enc("1110111011010x"), PT, false), d), a);
    at(at(at(w, 45, 1), 46, 1), 48, 4)
}
fn flo(d: u64, b: u64) -> u64 {
    rb(rd(guard(enc("0101110000110x"), PT, false), d), b)
}
fn vote(d: u64, mode: u64, src_pred: u64) -> u64 {
    let w = rd(guard(enc("0101000011011x"), PT, false), d);
    at(at(at(w, 39, src_pred), 45, PT), 48, mode)
}
fn xmad(d: u64, a: u64, b: u64, c: u64, cop: u64, flags: [u64; 4]) -> u64 {
    let [psl, mrg, hilo_a, hilo_b] = flags;
    let w = rb(ra(rd(guard(enc("0101101100xxxx"), PT, false), d), a), b);
    let w = at(at(at(w, 39, c), 50, cop), 36, psl);
    at(at(at(w, 37, mrg), 53, hilo_a), 35, hilo_b)
}
fn cal(address: u64, target: u64) -> InstOp {
    let rel = target.wrapping_sub(address + 8) & 0xff_ffff;
    // inc is bit 6
    get_op(address, enc("111000100110xx") | rel << 20 | 1 << 6)
}

/// A function as blocks of (predecessor count, operations).
struct Fn {
    address: u64,
    blocks: Vec<(usize, Vec<InstOp>)>,
}

fn plain(address: u64, words: &[u64]) -> Vec<InstOp> {
    words
        .iter()
        .enumerate()
        .map(|(i, w)| get_op(address + 8 * i as u64, *w))
        .collect()
}

fn get_address_v2() -> Vec<u64> {
    vec![
        s2r(10, 15),
        s2r(11, 28),
        shr_w(7, 10, 16),
        lop32i(12, 10, 0xfe00),
        iadd32i(6, 12, 0x200),
        lop32i(13, 10, 0xff),
        // BFI R14 = R11, 0x808, R13
        at(
            at(
                ra(rd(guard(enc("0011011x11110x"), PT, false), 14), 11),
                20,
                0x808,
            ),
            39,
            13,
        ),
        // ISCADD.CC R4 = R14 << 2 + c[0][404]
        at(
            at(
                at(
                    at(
                        ra(rd(guard(enc("0100110000011x"), PT, false), 4), 14),
                        20,
                        404,
                    ),
                    39,
                    2,
                ),
                34,
                0,
            ),
            47,
            1,
        ),
        // IADD.X R5 = RZ + c[0][405]
        at(
            at(
                ra(rd(guard(enc("0100110000010x"), PT, false), 5), RZ),
                20,
                405,
            ),
            43,
            1,
        ),
        ret(PT, false),
    ]
}

fn is_last_warp_thread() -> Vec<u64> {
    let lop = {
        // LOP.PASS_B R22 = RZ, ~R20 with the predicate output PT
        let w = rb(ra(rd(guard(enc("0101110001000x"), PT, false), 22), RZ), 20);
        at(at(at(w, 41, 3), 40, 1), 48, PT)
    };
    vec![
        s2r(20, 19),
        s2r(21, 0),
        lop,
        isetp(0, 22, RZ, 5),
        // VOTE.ANY R23, PT, P0
        vote(23, 1, 0),
        flo(24, 23),
        isetp(1, 24, 21, 2),
        ret(PT, false),
    ]
}

fn program(functions: Vec<Fn>) -> DecodedProgram {
    DecodedProgram {
        functions: functions
            .into_iter()
            .map(|f| DecodedFunction {
                address: f.address,
                blocks: f
                    .blocks
                    .into_iter()
                    .map(|(preds, ops)| Block {
                        address: ops.first().map_or(0, |o| o.address),
                        end_address: ops.last().map_or(0, |o| o.address + 8),
                        preds: vec![0; preds],
                        succs: vec![],
                        ops,
                        push_ops: vec![],
                        sync_targets: vec![],
                    })
                    .collect(),
            })
            .collect(),
        attribute_usage: AttributeUsage::default(),
        used_features: 0,
        clip_distances_written: 0,
        cb1_data_size: 0,
        size: 0,
    }
}

fn begin_function() -> Fn {
    let a = 0x200;
    let b1 = plain(
        a + 8,
        &[
            ret(1, true),
            ldg(30, 4),
            shr_w(31, 30, 16),
            isetp(2, 31, 7, 2),
            ret(2, false),
        ],
    );
    let loop_at = a + 8 + 5 * 8;
    let mut l = plain(
        loop_at,
        &[ldg(32, 4), shr_w(33, 32, 16), isetp(3, 33, 7, 2)],
    );
    let bra_at = loop_at + 3 * 8;
    let rel = loop_at.wrapping_sub(bra_at + 8) & 0xff_ffff;
    l.push(get_op(
        bra_at,
        guard(enc("111000100100xx"), 3, true) | 15 | rel << 20,
    ));
    Fn {
        address: a,
        blocks: vec![
            (0, vec![cal(a, 0x300)]),
            (1, b1),
            (2, l),
            (1, plain(bra_at + 8, &[ret(PT, false)])),
        ],
    }
}

fn end_function_words() -> Vec<u64> {
    let membar = at(guard(enc("1110111110011x"), PT, false), 8, 3);
    let red = {
        let w = ra(guard(enc("1110101111111x"), 5, false), 4);
        at(at(w, 48, 1), 0, 47)
    };
    vec![
        0,
        ret(1, true),
        membar,
        s2r(40, 19),
        isetp(4, 40, RZ, 5),
        ret(4, false),
        s2r(41, 0),
        vote(42, 0, PT),
        rb(rd(guard(enc("0101110000001x"), PT, false), 43), 42),
        flo(44, 42),
        isetp(5, 44, 41, 2),
        xmad(45, 6, 43, RZ, 0, [0, 1, 0, 1]),
        xmad(46, 6, 43, RZ, 0, [0, 0, 0, 0]),
        xmad(47, 6, 45, 46, 4, [1, 0, 1, 1]),
        red,
        ret(PT, false),
    ]
}

fn end_function() -> Fn {
    let a = 0x400;
    let words = end_function_words();
    Fn {
        address: a,
        blocks: vec![(0, vec![cal(a, 0x300)]), (1, plain(a + 8, &words[1..]))],
    }
}

fn fsi_program() -> DecodedProgram {
    let main = Fn {
        address: 0,
        blocks: vec![
            (0, vec![cal(0, 0x200)]),
            (1, vec![cal(8, 0x400)]),
            (1, plain(16, &[ret(PT, false)])),
        ],
    };
    let get_address = Fn {
        address: 0x100,
        blocks: vec![(0, plain(0x100, &get_address_v2()))],
    };
    let last = Fn {
        address: 0x300,
        blocks: vec![(0, plain(0x300, &is_last_warp_thread()))],
    };
    program(vec![
        main,
        get_address,
        begin_function(),
        last,
        end_function(),
    ])
}

#[test]
fn interlock_functions_are_recognised() {
    use FunctionType::*;
    assert_eq!(
        run_pass(&fsi_program()),
        vec![User, Unused, FsiBegin, Unused, FsiEnd]
    );
}

#[test]
fn programs_without_the_address_function_are_left_alone() {
    let mut p = fsi_program();
    p.functions.remove(1);
    assert!(run_pass(&p).iter().all(|t| *t == FunctionType::User));
}

#[test]
fn last_warp_thread_alone() {
    use super::fsi_match::{build_tree, matches_tree};
    use super::fsi_patterns::Patterns;
    let f = Fn {
        address: 0x300,
        blocks: vec![(0, plain(0x300, &is_last_warp_thread()))],
    };
    let p = program(vec![f]);
    let t = build_tree(&p.functions[0].blocks);
    let pat = Patterns::build();
    assert!(matches_tree(&pat, &pat.is_last_warp_thread, &t, None));
}

/// Lays the functions out as a compute program (scheduling word first and after every three words).
fn compute_code(functions: &[(usize, Vec<u64>)], size_words: usize) -> Vec<u8> {
    let mut words = vec![0u64; size_words];
    for (base, ops) in functions {
        for (k, w) in ops.iter().enumerate() {
            words[base + 1 + k + k / 3] = *w;
        }
    }
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn op_address(base: usize, k: usize) -> u64 {
    8 * (base + 1 + k + k / 3) as u64
}

fn cal_word(from: u64, to: u64) -> u64 {
    cal(from, to).raw
}

const EXIT: u64 = 0xe300_0000_0000_0000 | 0x70000 | 0xf;

#[test]
fn calls_become_interlock_instructions() {
    use crate::job::parse_jobs;
    let (get_address, begin, last, end) = (16usize, 32usize, 64usize, 80usize);
    let main_cals = [get_address, begin, end];
    let mut main: Vec<u64> = Vec::new();
    for (k, target) in main_cals.iter().enumerate() {
        main.push(cal_word(op_address(0, k), op_address(*target, 0) - 8));
    }
    main.push(EXIT);
    let last_ops = is_last_warp_thread();
    let ga = get_address_v2();
    let call_last = |base: usize| cal_word(op_address(base, 0), op_address(last, 0) - 8);
    // The begin function: CAL, RET @!P1, LDG, SHR, ISETP, RET @P2, then the loop.
    let loop_k = 5usize;
    let loop_addr = op_address(begin, loop_k);
    let bra_k = loop_k + 3;
    let rel = loop_addr.wrapping_sub(op_address(begin, bra_k) + 8) & 0xff_ffff;
    let begin_ops = vec![
        call_last(begin),
        ret(1, true),
        ldg(30, 4),
        shr_w(31, 30, 16),
        isetp(2, 31, 7, 2),
        ret(2, false),
        ldg(32, 4),
        shr_w(33, 32, 16),
        isetp(3, 33, 7, 2),
        guard(enc("111000100100xx"), 3, true) | 15 | rel << 20,
        ret(PT, false),
    ];
    let end_ops = {
        let mut e = end_function_words();
        e[0] = call_last(end);
        e
    };
    let code = compute_code(
        &[
            (0, main),
            (get_address, ga),
            (begin, begin_ops),
            (last, last_ops),
            (end, end_ops),
        ],
        112,
    );
    let jobs = parse_jobs(r#"[{"key": "t", "stage": "cs", "local": [1, 1, 1]}]"#).expect("jobs");
    let text = crate::front::pipeline::emit_text(&jobs[0], &code).expect("emit");
    // op 3e is FSIBegin and op 3f is FSIEnd.
    assert_eq!(text.matches("op 3e ").count(), 1, "{text}");
    assert_eq!(text.matches("op 3f ").count(), 1, "{text}");
}
