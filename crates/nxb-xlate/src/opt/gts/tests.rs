//! Tests of the raw word load grouping (`plan_group` and the base decomposition).

use super::*;

/// `{x, y} + k` the way the front end writes it: the low word `x + k`, the high word `y` plus 1 where the sum is
/// below `x`.
fn plus(ir: &mut Ir, x: Opd, y: Opd, k: i32, compare_with: Option<Opd>) -> (Opd, Opd) {
    let (lo, ck) = (ir.local(), ir.const_(k));
    ir.new_op(Inst::Add, StorageKind::None, Some(lo), &[x, ck]);
    let (wrapped, one, zero, carry, hi) = (
        ir.local(),
        ir.const_(1),
        ir.const_(0),
        ir.local(),
        ir.local(),
    );
    let against = compare_with.unwrap_or(x);
    ir.new_op(
        Inst::CompareLessU32,
        StorageKind::None,
        Some(wrapped),
        &[lo, against],
    );
    ir.new_op(
        Inst::ConditionalSelect,
        StorageKind::None,
        Some(carry),
        &[wrapped, one, zero],
    );
    ir.new_op(Inst::Add, StorageKind::None, Some(hi), &[y, carry]);
    (lo, hi)
}

#[test]
fn a_constant_add_with_its_carry_is_the_base_plus_the_constant() {
    let mut ir = Ir::default();
    let (x, y) = (ir.local(), ir.local());
    let (lo, hi) = plus(&mut ir, x, y, 20, None);
    assert_eq!(decompose(&ir, lo, hi), (x, y, 20));
    // The sum of two adds: the base stays the first one
    let (lo2, hi2) = plus(&mut ir, lo, hi, 8, None);
    assert_eq!(decompose(&ir, lo2, hi2), (x, y, 28));
}

#[test]
fn an_address_without_the_carry_is_its_own_base() {
    let mut ir = Ir::default();
    let (x, y, z) = (ir.local(), ir.local(), ir.local());
    // The carry out of a different sum does not make `x + 4` a 64 bit add
    let (lo, hi) = plus(&mut ir, x, y, 4, Some(z));
    assert_eq!(decompose(&ir, lo, hi), (lo, hi, 0));
    // A plain pair
    assert_eq!(decompose(&ir, x, y), (x, y, 0));
}

#[test]
fn a_negative_constant_is_not_a_group_offset() {
    let mut ir = Ir::default();
    let (x, y) = (ir.local(), ir.local());
    let (lo, hi) = plus(&mut ir, x, y, -4, None);
    assert_eq!(decompose(&ir, lo, hi), (lo, hi, 0));
}

#[test]
fn two_adds_of_one_base_meet_in_one_base() {
    let mut ir = Ir::default();
    let (x, y) = (ir.local(), ir.local());
    let (a_lo, a_hi) = plus(&mut ir, x, y, 16, None);
    let (b_lo, b_hi) = plus(&mut ir, x, y, 1000, None);
    let (a, b) = (decompose(&ir, a_lo, a_hi), decompose(&ir, b_lo, b_hi));
    assert_eq!((a.0, a.1, b.0, b.1), (x, y, x, y));
    assert_eq!((a.2, b.2), (16, 1000));
}
