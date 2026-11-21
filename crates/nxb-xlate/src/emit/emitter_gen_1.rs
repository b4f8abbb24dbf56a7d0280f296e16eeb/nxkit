//! Produced by tools/gen_xlate_emitter_insts.py from EmitterContextInsts.cs: do not edit.
#![allow(clippy::too_many_arguments, clippy::needless_lifetimes)]
use super::emitter::{EmitterContext, FP32};
use crate::ir::inst::{Inst, StorageKind};
use crate::ir::ops::Opd;

impl EmitterContext<'_, '_> {
    pub fn fp_modulo(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Modulo, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_modulo_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Modulo, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_multiply(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Multiply, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_multiply_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Multiply, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_negate(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Negate, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_negate_t(&mut self, a: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Negate, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_reciprocal_square_root(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::ReciprocalSquareRoot, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_reciprocal_square_root_t(&mut self, a: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::ReciprocalSquareRoot, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_round(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Round, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_round_t(&mut self, a: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Round, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_sine(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(FP32 | Inst::Sine, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_square_root(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(FP32 | Inst::SquareRoot, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_subtract(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Subtract, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_subtract_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Subtract, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_truncate(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Truncate, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_truncate_t(&mut self, a: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Truncate, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_swizzle_add(&mut self, a: Opd, b: Opd, mask: i32) -> Opd {
        let t0 = self.ir.const_(mask);
        let dest = self.ir.local();
        self.add_op(Inst::SwizzleAdd, StorageKind::None, Some(dest), &[a, b, t0]);
        dest
    }

    pub fn group_memory_barrier(&mut self) {
        self.add_op(Inst::GroupMemoryBarrier, StorageKind::None, None, &[]);
    }

    pub fn i_absolute(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Absolute, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn i_add(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Add, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_clamp_s32(&mut self, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Clamp, StorageKind::None, Some(dest), &[a, b, c]);
        dest
    }

    pub fn i_clamp_u32(&mut self, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ClampU32, StorageKind::None, Some(dest), &[a, b, c]);
        dest
    }

    pub fn i_compare_equal(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareEqual, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_greater(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareGreater, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_greater_or_equal(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareGreaterOrEqual, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_greater_or_equal_unsigned(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareGreaterOrEqualU32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_greater_unsigned(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareGreaterU32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_less(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareLess, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_less_or_equal(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareLessOrEqual, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_less_or_equal_unsigned(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareLessOrEqualU32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_less_unsigned(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareLessU32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_compare_not_equal(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::CompareNotEqual, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_convert_s32_to_fp32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertS32ToFP32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn i_convert_s32_to_fp64(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertS32ToFP64, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn i_convert_u32_to_fp32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertU32ToFP32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn i_convert_u32_to_fp64(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertU32ToFP64, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn i_maximum_s32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Maximum, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_maximum_u32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::MaximumU32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_minimum_s32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Minimum, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_minimum_u32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::MinimumU32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_multiply(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Multiply, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn i_negate(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Negate, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn i_subtract(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Subtract, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn is_nan(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::IsNan, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn is_nan_t(&mut self, a: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::IsNan, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn load_o2(&mut self, storage_kind: StorageKind, e0: Opd, e1: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Load, storage_kind, Some(dest), &[e0, e1]);
        dest
    }

    pub fn load_b0(&mut self, storage_kind: StorageKind, binding: i32) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::Load, storage_kind, Some(dest), &[t0]);
        dest
    }

    pub fn load_b1(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::Load, storage_kind, Some(dest), &[t0, e0]);
        dest
    }

    pub fn load_b2(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::Load, storage_kind, Some(dest), &[t0, e0, e1]);
        dest
    }

    pub fn load_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, e2: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::Load, storage_kind, Some(dest), &[t0, e0, e1, e2]);
        dest
    }

    pub fn memory_barrier(&mut self) {
        self.add_op(Inst::MemoryBarrier, StorageKind::None, None, &[]);
    }

    pub fn multiply_high_s32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::MultiplyHighS32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn multiply_high_u32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::MultiplyHighU32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn pack_double2x32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::PackDouble2x32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn pack_half2x16(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::PackHalf2x16, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn shift_left(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ShiftLeft, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn shift_right_s32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ShiftRightS32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn shift_right_u32(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ShiftRightU32, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn shuffle(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Shuffle, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn shuffle_down(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ShuffleDown, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn shuffle_up(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ShuffleUp, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn shuffle_xor(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ShuffleXor, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn store_o3(&mut self, storage_kind: StorageKind, e0: Opd, e1: Opd, value: Opd) {
        self.add_op(Inst::Store, storage_kind, None, &[e0, e1, value]);
    }

    pub fn store_b1(&mut self, storage_kind: StorageKind, binding: i32, value: Opd) {
        let t0 = self.ir.const_(binding);
        self.add_op(Inst::Store, storage_kind, None, &[t0, value]);
    }

    pub fn store_b2(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, value: Opd) {
        let t0 = self.ir.const_(binding);
        self.add_op(Inst::Store, storage_kind, None, &[t0, e0, value]);
    }

    pub fn store_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) {
        let t0 = self.ir.const_(binding);
        self.add_op(Inst::Store, storage_kind, None, &[t0, e0, e1, value]);
    }

    pub fn vote_all(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::VoteAll, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn vote_all_equal(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::VoteAllEqual, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn vote_any(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::VoteAny, StorageKind::None, Some(dest), &[a]);
        dest
    }

}
