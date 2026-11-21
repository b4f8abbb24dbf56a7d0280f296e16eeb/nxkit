//! Address and small-integer helpers of the memory emitters (`InstEmitMemory.cs`).

use super::hlp::REGISTER_ZERO_INDEX;
use crate::emit::emitter::EmitterContext;
use crate::ir::ops::{Opd, regtype};
use crate::maxwell::enums::LsSize;

pub(super) fn get_40_bits_address(
    ctx: &mut EmitterContext<'_, '_>,
    ra: i32,
    extended: bool,
    offset: i32,
) -> (Opd, Opd) {
    let mut addr_low = ctx.ir.register(ra, regtype::GPR);
    let mut addr_high = if extended && ra != REGISTER_ZERO_INDEX {
        ctx.ir.register(ra + 1, regtype::GPR)
    } else {
        ctx.ir.const_(0)
    };
    let offs = ctx.ir.const_(offset);
    addr_low = ctx.i_add(addr_low, offs);
    if extended {
        let carry = ctx.i_compare_less_unsigned(addr_low, offs);
        let one = ctx.ir.const_(1);
        let zero = ctx.ir.const_(0);
        let sel = ctx.conditional_select(carry, one, zero);
        addr_high = ctx.i_add(addr_high, sel);
    }
    (addr_low, addr_high)
}

pub(crate) fn get_bit_offset(ctx: &mut EmitterContext<'_, '_>, base_offset: Opd) -> Opd {
    let c3 = ctx.ir.const_(3);
    let m = ctx.bitwise_and(base_offset, c3);
    let c3b = ctx.ir.const_(3);
    ctx.shift_left(m, c3b)
}

pub(crate) fn extract_small_int(
    ctx: &mut EmitterContext<'_, '_>,
    size: i32,
    bit_offset: Opd,
    value: Opd,
) -> Opd {
    let mut value = ctx.shift_right_u32(value, bit_offset);
    match LsSize(size) {
        LsSize::U8 => value = zero_extend_to_32(ctx, value, 8),
        LsSize::U16 => value = zero_extend_to_32(ctx, value, 16),
        LsSize::S8 => value = sign_extend_to_32(ctx, value, 8),
        LsSize::S16 => value = sign_extend_to_32(ctx, value, 16),
        _ => {}
    }
    value
}

fn zero_extend_to_32(ctx: &mut EmitterContext<'_, '_>, src: Opd, bits: i32) -> Opd {
    let mask = (u32::MAX >> (32 - bits)) as i32;
    let m = ctx.ir.const_(mask);
    ctx.bitwise_and(src, m)
}

fn sign_extend_to_32(ctx: &mut EmitterContext<'_, '_>, src: Opd, bits: i32) -> Opd {
    let z = ctx.ir.const_(0);
    let b = ctx.ir.const_(bits);
    ctx.bitfield_extract_s32(src, z, b)
}

pub(super) fn insert_small_int(
    ctx: &mut EmitterContext<'_, '_>,
    size: i32,
    bit_offset: Opd,
    word: Opd,
    value: Opd,
) -> Opd {
    let mut value = value;
    match LsSize(size) {
        LsSize::U8 | LsSize::S8 => {
            let m = ctx.ir.const_(0xff);
            value = ctx.bitwise_and(value, m);
            let w = ctx.ir.const_(8);
            value = ctx.bitfield_insert(word, value, bit_offset, w);
        }
        LsSize::U16 | LsSize::S16 => {
            let m = ctx.ir.const_(0xffff);
            value = ctx.bitwise_and(value, m);
            let w = ctx.ir.const_(16);
            value = ctx.bitfield_insert(word, value, bit_offset, w);
        }
        _ => {}
    }
    value
}
