//! The console variable finder on synthetic code (ports of the cases of `dm/tools/tests/test_ue_cvar.py` at
//! `034e91a8`): UE 4.18's constructor form, UE 4.2x / 5's register call, UE 4.18's inlined form with a page base made
//! far before the name, and the paths that skip that base.

use super::*;

const TEXT: usize = 0x8000;
const RO: usize = 0x8000;
const DATA: usize = 0xa000;
const END: usize = 0xc000;
const NAME: usize = 0x8100;
const OBJ: i64 = 0xa5c8;
const OBJ2: i64 = 0xa700;
const CTOR: usize = 0x6000;
/// The inlined form's page base (adrp x22 + add x22, x22, #0x100).
const BASE: i64 = 0xa100;

fn adrp(d: u32, pc: usize, target: i64) -> u32 {
    let imm = (target >> 12) - (pc as i64 >> 12);
    0x9000_0000 | ((imm as u32 & 3) << 29) | (((imm >> 2) as u32 & 0x7_FFFF) << 5) | d
}
fn addi(d: u32, n: u32, imm: i64) -> u32 {
    0x9100_0000 | ((imm as u32) << 10) | (n << 5) | d
}
fn b(from: usize, to: usize) -> u32 {
    0x1400_0000 | (((to as i64 - from as i64) / 4) as u32 & 0x3FF_FFFF)
}
fn bl(from: usize, to: usize) -> u32 {
    b(from, to) | 0x8000_0000
}
fn blr(r: u32) -> u32 {
    0xD63F_0000 | (r << 5)
}
fn ldrx(t: u32, n: u32) -> u32 {
    0xF940_0000 | (n << 5) | t
}
fn strx(t: u32, n: u32, k: i64) -> u32 {
    0xF900_0000 | ((k as u32) << 10) | (n << 5) | t
}
fn mov(d: u32, s: u32) -> u32 {
    0xAA00_03E0 | (s << 16) | d
}

/// UE 4.2x / 5: the name into x1, blr (register), then the object into x19, blr, `str x0, [x19, #0x10]`.
fn register_form(at: usize, obj: i64) -> Vec<u32> {
    let mut w = vec![
        ldrx(8, 0),
        adrp(1, at + 4, NAME as i64),
        addi(1, 1, NAME as i64 & 0xfff),
        blr(8),
    ];
    let a = at + 4 * w.len();
    w.extend([
        adrp(19, a, obj),
        addi(19, 19, obj & 0xfff),
        ldrx(8, 0),
        blr(8),
        strx(0, 19, 2),
        RET,
    ]);
    w
}

/// UE 4.18: the object into x19, the name into x1, x0 = x19, bl the constructor.
fn ctor_form(at: usize, obj: i64) -> Vec<u32> {
    vec![
        adrp(19, at, obj),
        addi(19, 19, obj & 0xfff),
        adrp(1, at + 8, NAME as i64),
        addi(1, 1, NAME as i64 & 0xfff),
        mov(0, 19),
        bl(at + 20, CTOR),
        RET,
    ]
}

#[derive(Clone, Copy, Default)]
struct Inline {
    no_vtable: bool,
    clobber: bool,
    far: bool,
}

/// UE 4.18 inlined (OCTOPATH TRAVELER): x22 = the page base, made once, then a forward b over an else block, then the
/// name into x1, blr (register), `str x0, [x22, #k + 8]`, `str x8, [x22, #k]` (the vtable), blr, `str x0, [x22, #k + 0x10]`.
fn inline_form(at: usize, obj: i64, o: Inline) -> Vec<u32> {
    let k = obj - BASE;
    let mut w = vec![
        adrp(22, at, BASE),
        addi(22, 22, BASE & 0xfff),
        0,
        mov(0, 1),
        mov(2, 3),
    ];
    w.push(if o.clobber { mov(22, 0) } else { mov(4, 5) });
    let n = w.len();
    w.extend([
        ldrx(8, 0),
        adrp(1, at + 4 * (n + 1), NAME as i64),
        addi(1, 1, NAME as i64 & 0xfff),
        blr(8),
    ]);
    w.push(strx(0, 22, (k + 8) / 8));
    if !o.no_vtable {
        w.push(strx(8, 22, k / 8));
    }
    w.extend([ldrx(8, 0), blr(8), strx(0, 22, (k + 0x10) / 8), RET]);
    w[2] = b(at + 8, at + 4 * if o.far { w.len() + 4 } else { 4 });
    w
}

fn image(seqs: &[&dyn Fn(usize) -> Vec<u32>], wide: bool) -> Vec<u8> {
    let mut img = vec![0u8; END];
    let z = if wide { 4 } else { 2 };
    for (i, c) in "t.MaxFPS".bytes().enumerate() {
        img[NAME + i * z] = c;
    }
    img[CTOR..CTOR + 4].copy_from_slice(&RET.to_le_bytes());
    let mut at = 0x1000;
    for s in seqs {
        let words = s(at);
        for (i, w) in words.iter().enumerate() {
            img[at + 4 * i..at + 4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
        at += 4 * words.len() + 0x40;
    }
    img
}

fn find(img: &[u8]) -> Option<i64> {
    cvar_find(img, TEXT, RO, DATA, "t.MaxFPS").0
}

#[test]
fn the_constructor_and_the_register_forms_are_found() {
    assert_eq!(find(&image(&[&|a| ctor_form(a, OBJ)], false)), Some(OBJ));
    assert_eq!(find(&image(&[&|a| register_form(a, OBJ)], true)), Some(OBJ));
}

#[test]
fn the_inlined_form_is_found_from_its_page_base() {
    let img = image(&[&|a| inline_form(a, OBJ, Inline::default())], true);
    assert_eq!(find(&img), Some(OBJ));
}

#[test]
fn the_inlined_form_needs_one_way_in() {
    for o in [
        Inline {
            clobber: true,
            ..Inline::default()
        },
        Inline {
            no_vtable: true,
            ..Inline::default()
        },
        Inline {
            far: true,
            ..Inline::default()
        },
    ] {
        assert_eq!(find(&image(&[&|a| inline_form(a, OBJ, o)], false)), None);
    }
    // A branch from elsewhere into the span of the second sequence: a path to the store that skips the adrp.
    let img = image(
        &[&|a| vec![b(a, 0x1048 + 4 * 6), RET], &|a| {
            inline_form(a, OBJ, Inline::default())
        }],
        false,
    );
    assert_eq!(find(&img), None);
}

#[test]
fn a_name_registered_twice_is_red() {
    let img = image(
        &[&|a| register_form(a, OBJ), &|a| register_form(a, OBJ2)],
        false,
    );
    let (o, why) = cvar_find(&img, TEXT, RO, DATA, "t.MaxFPS");
    assert_eq!(o, None);
    assert!(
        why.starts_with("[RED] ") && why.contains("2 objects"),
        "{why}"
    );
}
