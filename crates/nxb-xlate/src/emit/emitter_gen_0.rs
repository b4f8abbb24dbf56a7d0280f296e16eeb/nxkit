//! Produced by tools/gen_xlate_emitter_insts.py from EmitterContextInsts.cs: do not edit.
#![allow(clippy::too_many_arguments, clippy::needless_lifetimes)]
use super::emitter::{EmitterContext, FP32};
use crate::ir::inst::{Inst, StorageKind};
use crate::ir::ops::Opd;

impl EmitterContext<'_, '_> {
    pub fn atomic_add_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicAdd, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_and_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicAnd, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_compare_and_swap_o4(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd, d: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicCompareAndSwap, storage_kind, Some(dest), &[a, b, c, d]);
        dest
    }

    pub fn atomic_max_s32_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicMaxS32, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_max_u32_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicMaxU32, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_min_s32_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicMinS32, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_min_u32_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicMinU32, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_or_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicOr, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_swap_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicSwap, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_xor_o3(&mut self, storage_kind: StorageKind, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::AtomicXor, storage_kind, Some(dest), &[a, b, c]);
        dest
    }

    pub fn atomic_add_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicAdd, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn atomic_and_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicAnd, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn atomic_compare_and_swap_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, compare: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicCompareAndSwap, storage_kind, Some(dest), &[t0, e0, compare, value]);
        dest
    }

    pub fn atomic_compare_and_swap_b4(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, compare: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicCompareAndSwap, storage_kind, Some(dest), &[t0, e0, e1, compare, value]);
        dest
    }

    pub fn atomic_max_s32_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicMaxS32, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn atomic_max_u32_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicMaxU32, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn atomic_min_s32_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicMinS32, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn atomic_min_u32_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicMinU32, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn atomic_or_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicOr, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn atomic_swap_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicSwap, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn atomic_xor_b3(&mut self, storage_kind: StorageKind, binding: i32, e0: Opd, e1: Opd, value: Opd) -> Opd {
        let t0 = self.ir.const_(binding);
        let dest = self.ir.local();
        self.add_op(Inst::AtomicXor, storage_kind, Some(dest), &[t0, e0, e1, value]);
        dest
    }

    pub fn subgroup_barrier(&mut self) {
        self.add_op(Inst::SubgroupBarrier, StorageKind::None, None, &[]);
    }

    pub fn bit_count(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitCount, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn bitfield_extract_s32(&mut self, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitfieldExtractS32, StorageKind::None, Some(dest), &[a, b, c]);
        dest
    }

    pub fn bitfield_extract_u32(&mut self, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitfieldExtractU32, StorageKind::None, Some(dest), &[a, b, c]);
        dest
    }

    pub fn bitfield_insert(&mut self, a: Opd, b: Opd, c: Opd, d: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitfieldInsert, StorageKind::None, Some(dest), &[a, b, c, d]);
        dest
    }

    pub fn bitfield_reverse(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitfieldReverse, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn bitwise_and(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitwiseAnd, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn bitwise_exclusive_or(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitwiseExclusiveOr, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn bitwise_not(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitwiseNot, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn bitwise_or(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::BitwiseOr, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn branch(&mut self, d: Opd) {
        self.add_op(Inst::Branch, StorageKind::None, Some(d), &[]);
    }

    pub fn branch_if_false(&mut self, d: Opd, a: Opd) {
        self.add_op(Inst::BranchIfFalse, StorageKind::None, Some(d), &[a]);
    }

    pub fn branch_if_true(&mut self, d: Opd, a: Opd) {
        self.add_op(Inst::BranchIfTrue, StorageKind::None, Some(d), &[a]);
    }

    pub fn conditional_select(&mut self, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConditionalSelect, StorageKind::None, Some(dest), &[a, b, c]);
        dest
    }

    pub fn copy(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::Copy, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn discard(&mut self) {
        self.add_op(Inst::Discard, StorageKind::None, None, &[]);
    }

    pub fn find_lsb(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::FindLSB, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn find_msbs32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::FindMSBS32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn find_msbu32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::FindMSBU32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp32_convert_to_fp64(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertFP32ToFP64, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp64_convert_to_fp32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertFP64ToFP32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_absolute(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Absolute, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_absolute_t(&mut self, a: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Absolute, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_add(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Add, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_add_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Add, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_ceiling(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Ceiling, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_ceiling_t(&mut self, a: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Ceiling, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_compare_equal(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::CompareEqual, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_compare_equal_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::CompareEqual, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_compare_less(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::CompareLess, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_compare_less_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::CompareLess, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp32_convert_to_s32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertFP32ToS32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp32_convert_to_u32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertFP32ToU32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp64_convert_to_s32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertFP64ToS32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp64_convert_to_u32(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst::ConvertFP64ToU32, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_cosine(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(FP32 | Inst::Cosine, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_divide(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Divide, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_divide_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Divide, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_exponent_b2(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(FP32 | Inst::ExponentB2, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_floor(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Floor, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_floor_t(&mut self, a: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Floor, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_fused_multiply_add(&mut self, a: Opd, b: Opd, c: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::FusedMultiplyAdd, StorageKind::None, Some(dest), &[a, b, c]);
        dest
    }

    pub fn fp_fused_multiply_add_t(&mut self, a: Opd, b: Opd, c: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::FusedMultiplyAdd, StorageKind::None, Some(dest), &[a, b, c]);
        dest
    }

    pub fn fp_logarithm_b2(&mut self, a: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(FP32 | Inst::LogarithmB2, StorageKind::None, Some(dest), &[a]);
        dest
    }

    pub fn fp_maximum(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Maximum, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_maximum_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Maximum, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_minimum(&mut self, a: Opd, b: Opd) -> Opd {
        let dest = self.ir.local();
        self.add_op(Inst(Inst::FP32) | Inst::Minimum, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

    pub fn fp_minimum_t(&mut self, a: Opd, b: Opd, fp_type: Inst) -> Opd {
        let dest = self.ir.local();
        self.add_op(fp_type | Inst::Minimum, StorageKind::None, Some(dest), &[a, b]);
        dest
    }

}
