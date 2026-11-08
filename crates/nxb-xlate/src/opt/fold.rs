//! `ConstantFolding`.
use super::Resources;
use crate::ir::{FALSE, Inst, Ir, NodeId, OperandType, StorageKind, TRUE};

pub const F32: u32 = Inst::FP32;

pub fn fp(i: Inst) -> Inst {
    Inst(F32 | i.0)
}

fn b(v: bool) -> i32 {
    if v { TRUE } else { FALSE }
}

/// `value.Extract(lsb, length)` on an int.
pub fn extract(value: i32, lsb: i32, length: i32) -> i32 {
    let v = (value as u32).wrapping_shr(lsb as u32 & 31);
    (v & (!0u32).wrapping_shr((32 - length) as u32 & 31)) as i32
}

/// `BitConverter.UInt16BitsToHalf` widened to float.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = if exp == 0 {
        if man == 0 {
            sign << 31
        } else {
            // subnormal: normalise
            let mut e = 127 - 15 + 1;
            let mut m = man;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            (sign << 31) | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
    } else if exp == 31 {
        (sign << 31) | (0xff << 23) | (man << 13)
    } else {
        (sign << 31) | ((exp + 127 - 15) << 23) | (man << 13)
    };
    f32::from_bits(bits)
}

fn clamp_f(x: f32, lo: f32, hi: f32) -> Result<f32, String> {
    if lo > hi {
        return Err("Math.Clamp: min greater than max".into());
    }
    Ok(if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    })
}

pub fn run_pass(ir: &mut Ir, res: &dyn Resources, n: NodeId) -> Result<(), String> {
    if !ir.is_op(n) {
        return Ok(());
    }
    let srcs = ir.op(n).sources.clone();
    if !srcs
        .iter()
        .all(|s| s.is_some_and(|o| ir.ty(o) == OperandType::Constant))
    {
        return Ok(());
    }
    let v: Vec<i32> = srcs.iter().flatten().map(|o| ir.value(*o)).collect();
    let f = |i: usize| f32::from_bits(v[i] as u32);
    let inst = ir.op(n).inst;
    let kind = ir.op(n).storage_kind;
    let need = |k: usize| -> Result<(), String> {
        if v.len() < k {
            Err("constant folding: too few sources".into())
        } else {
            Ok(())
        }
    };
    enum R {
        I(i32),
        F(f32),
        None,
    }
    let r: R = match inst {
        Inst::Add => {
            need(2)?;
            R::I(v[0].wrapping_add(v[1]))
        }
        Inst::BitCount => {
            need(1)?;
            R::I(v[0].count_ones() as i32)
        }
        Inst::BitwiseAnd => {
            need(2)?;
            R::I(v[0] & v[1])
        }
        Inst::BitwiseExclusiveOr => {
            need(2)?;
            R::I(v[0] ^ v[1])
        }
        Inst::BitwiseNot => {
            need(1)?;
            R::I(!v[0])
        }
        Inst::BitwiseOr => {
            need(2)?;
            R::I(v[0] | v[1])
        }
        Inst::BitfieldExtractS32 => {
            need(3)?;
            let value = extract(v[0], v[1], v[2]);
            let shift = 32 - v[2];
            R::I(value.wrapping_shl(shift as u32).wrapping_shr(shift as u32))
        }
        Inst::BitfieldExtractU32 => {
            need(3)?;
            R::I(extract(v[0], v[1], v[2]))
        }
        Inst::Clamp => {
            need(3)?;
            if v[1] > v[2] {
                return Err("Math.Clamp: min greater than max".into());
            }
            R::I(v[0].clamp(v[1], v[2]))
        }
        Inst::ClampU32 => {
            need(3)?;
            if (v[1] as u32) > (v[2] as u32) {
                return Err("Math.Clamp: min greater than max".into());
            }
            R::I((v[0] as u32).clamp(v[1] as u32, v[2] as u32) as i32)
        }
        Inst::CompareEqual => {
            need(2)?;
            R::I(b(v[0] == v[1]))
        }
        Inst::CompareGreater => {
            need(2)?;
            R::I(b(v[0] > v[1]))
        }
        Inst::CompareGreaterOrEqual => {
            need(2)?;
            R::I(b(v[0] >= v[1]))
        }
        Inst::CompareGreaterOrEqualU32 => {
            need(2)?;
            R::I(b(v[0] as u32 >= v[1] as u32))
        }
        Inst::CompareGreaterU32 => {
            need(2)?;
            R::I(b(v[0] as u32 > v[1] as u32))
        }
        Inst::CompareLess => {
            need(2)?;
            R::I(b(v[0] < v[1]))
        }
        Inst::CompareLessOrEqual => {
            need(2)?;
            R::I(b(v[0] <= v[1]))
        }
        Inst::CompareLessOrEqualU32 => {
            need(2)?;
            R::I(b(v[0] as u32 <= v[1] as u32))
        }
        Inst::CompareLessU32 => {
            need(2)?;
            R::I(b((v[0] as u32) < v[1] as u32))
        }
        Inst::CompareNotEqual => {
            need(2)?;
            R::I(b(v[0] != v[1]))
        }
        Inst::Divide => {
            need(2)?;
            if v[1] == 0 {
                R::I(0)
            } else {
                R::I(v[0].checked_div(v[1]).ok_or("integer division overflow")?)
            }
        }
        x if x == fp(Inst::Add) => {
            need(2)?;
            R::F(f(0) + f(1))
        }
        x if x == fp(Inst::Clamp) => {
            need(3)?;
            R::F(clamp_f(f(0), f(1), f(2))?)
        }
        x if x == fp(Inst::CompareEqual) => {
            need(2)?;
            R::I(b(f(0) == f(1)))
        }
        x if x == fp(Inst::CompareGreater) => {
            need(2)?;
            R::I(b(f(0) > f(1)))
        }
        x if x == fp(Inst::CompareGreaterOrEqual) => {
            need(2)?;
            R::I(b(f(0) >= f(1)))
        }
        x if x == fp(Inst::CompareLess) => {
            need(2)?;
            R::I(b(f(0) < f(1)))
        }
        x if x == fp(Inst::CompareLessOrEqual) => {
            need(2)?;
            R::I(b(f(0) <= f(1)))
        }
        x if x == fp(Inst::CompareNotEqual) => {
            need(2)?;
            R::I(b(f(0) != f(1)))
        }
        x if x == fp(Inst::Divide) => {
            need(2)?;
            R::F(f(0) / f(1))
        }
        x if x == fp(Inst::Multiply) => {
            need(2)?;
            R::F(f(0) * f(1))
        }
        x if x == fp(Inst::Negate) => {
            need(1)?;
            R::F(-f(0))
        }
        x if x == fp(Inst::Subtract) => {
            need(2)?;
            R::F(f(0) - f(1))
        }
        x if x == Inst::IsNan || x == fp(Inst::IsNan) => {
            need(1)?;
            R::I(b(f(0).is_nan()))
        }
        Inst::Load => {
            if kind == StorageKind::ConstantBuffer && v.len() == 4 {
                let (binding, field) = (v[0], v[1]);
                if let Some(slot) = res.try_get_constant_buffer_slot(binding)
                    && field == 0
                {
                    let off = v[2] * 4 + v[3];
                    let c = ir.cbuf(slot, off);
                    ir.turn_into_copy(n, c);
                }
            }
            R::None
        }
        Inst::Maximum => {
            need(2)?;
            R::I(v[0].max(v[1]))
        }
        Inst::MaximumU32 => {
            need(2)?;
            R::I((v[0] as u32).max(v[1] as u32) as i32)
        }
        Inst::Minimum => {
            need(2)?;
            R::I(v[0].min(v[1]))
        }
        Inst::MinimumU32 => {
            need(2)?;
            R::I((v[0] as u32).min(v[1] as u32) as i32)
        }
        Inst::Multiply => {
            need(2)?;
            R::I(v[0].wrapping_mul(v[1]))
        }
        Inst::Negate => {
            need(1)?;
            R::I(v[0].wrapping_neg())
        }
        Inst::ShiftLeft => {
            need(2)?;
            R::I(v[0].wrapping_shl(v[1] as u32))
        }
        Inst::ShiftRightS32 => {
            need(2)?;
            R::I(v[0].wrapping_shr(v[1] as u32))
        }
        Inst::ShiftRightU32 => {
            need(2)?;
            R::I((v[0] as u32).wrapping_shr(v[1] as u32) as i32)
        }
        Inst::Subtract => {
            need(2)?;
            R::I(v[0].wrapping_sub(v[1]))
        }
        Inst::UnpackHalf2x16 => {
            need(1)?;
            let index = ir.op(n).index;
            let value = v[0].wrapping_shr((index.wrapping_mul(16)) as u32) & 0xffff;
            R::F(half_to_f32(value as u16))
        }
        _ => R::None,
    };
    match r {
        R::I(x) => {
            let c = ir.const_(x);
            ir.turn_into_copy(n, c);
        }
        R::F(x) => {
            let c = ir.const_f(x);
            ir.turn_into_copy(n, c);
        }
        R::None => {}
    }
    Ok(())
}
