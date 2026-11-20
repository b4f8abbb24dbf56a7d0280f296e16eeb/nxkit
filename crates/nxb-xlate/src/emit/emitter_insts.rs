//! The multi-statement members of `EmitterContextInsts` (the one-line wrappers are generated in `emitter_gen.rs`).
//!
//! Naming of overloads: `_if` takes a flag and applies the operation only when it is set, `_t` takes an explicit `fpType`
//! (the plain name uses FP32), `load_io*`/`store_io*` are the `IoVariable` overloads, `_e` adds an element index and `_a`
//! an array and element index.

use super::emitter::{EmitterContext, FP32, FP64};
use crate::accessor::SetBinding;
use crate::ir::inst::{Inst, IoVariable, StorageKind};
use crate::ir::ops::{Opd, texflags};
use crate::types::SamplerType;

impl EmitterContext<'_, '_> {
    pub fn ballot(&mut self, a: Opd, index: i32) -> Opd {
        let dest = self.ir.local();
        let n = self
            .ir
            .new_op_multi(Inst::Ballot, index, &[Some(dest)], &[a]);
        self.add_node(n);
        dest
    }

    pub fn barrier(&mut self) {
        self.add_op(Inst::Barrier, StorageKind::None, None, &[]);
    }

    pub fn bitwise_not_if(&mut self, a: Opd, invert: bool) -> Opd {
        if invert { self.bitwise_not(a) } else { a }
    }

    /// `Call(funcId, returns, args)`.
    pub fn call(&mut self, func_id: i32, returns: bool, args: &[Opd]) -> Option<Opd> {
        let id = self.ir.const_(func_id);
        let mut all = vec![id];
        all.extend_from_slice(args);
        let dest = if returns { Some(self.ir.local()) } else { None };
        self.add_op(Inst::Call, StorageKind::None, dest, &all);
        dest
    }

    pub fn return_(&mut self) {
        self.add_op(Inst::Return, StorageKind::None, None, &[]);
    }

    pub fn return_value(&mut self, v: Opd) {
        self.add_op(Inst::Return, StorageKind::None, None, &[v]);
    }

    pub fn fp_abs_neg_t(&mut self, a: Opd, abs: bool, neg: bool, t: Inst) -> Opd {
        let a = self.fp_absolute_if_t(a, abs, t);
        self.fp_negate_if_t(a, neg, t)
    }

    pub fn fp_abs_neg(&mut self, a: Opd, abs: bool, neg: bool) -> Opd {
        self.fp_abs_neg_t(a, abs, neg, FP32)
    }

    pub fn fp_absolute_if_t(&mut self, a: Opd, abs: bool, t: Inst) -> Opd {
        if abs { self.fp_absolute_t(a, t) } else { a }
    }

    pub fn fp_absolute_if(&mut self, a: Opd, abs: bool) -> Opd {
        self.fp_absolute_if_t(a, abs, FP32)
    }

    pub fn fp_negate_if_t(&mut self, a: Opd, neg: bool, t: Inst) -> Opd {
        if neg { self.fp_negate_t(a, t) } else { a }
    }

    pub fn fp_negate_if(&mut self, a: Opd, neg: bool) -> Opd {
        self.fp_negate_if_t(a, neg, FP32)
    }

    pub fn fp_reciprocal_t(&mut self, a: Opd, t: Inst) -> Opd {
        let one = if t == FP64 {
            self.pack_double2x32_f(1.0)
        } else {
            self.ir.const_f(1.0)
        };
        self.fp_divide_t(one, a, t)
    }

    pub fn fp_reciprocal(&mut self, a: Opd) -> Opd {
        self.fp_reciprocal_t(a, FP32)
    }

    pub fn fp_saturate_t(&mut self, a: Opd, t: Inst) -> Opd {
        let (lo, hi) = if t == FP64 {
            (self.pack_double2x32_f(0.0), self.pack_double2x32_f(1.0))
        } else {
            (self.ir.const_f(0.0), self.ir.const_f(1.0))
        };
        let dest = self.ir.local();
        self.add_op(
            t | Inst(Inst::Clamp.0),
            StorageKind::None,
            Some(dest),
            &[a, lo, hi],
        );
        dest
    }

    pub fn fp_saturate(&mut self, a: Opd) -> Opd {
        self.fp_saturate_t(a, FP32)
    }

    pub fn fp_saturate_if_t(&mut self, a: Opd, sat: bool, t: Inst) -> Opd {
        if sat { self.fp_saturate_t(a, t) } else { a }
    }

    pub fn fp_saturate_if(&mut self, a: Opd, sat: bool) -> Opd {
        self.fp_saturate_if_t(a, sat, FP32)
    }

    pub fn fsi_begin(&mut self) {
        self.add_op(Inst::FSIBegin, StorageKind::None, None, &[]);
    }

    pub fn fsi_end(&mut self) {
        self.add_op(Inst::FSIEnd, StorageKind::None, None, &[]);
    }

    pub fn i_abs_neg(&mut self, a: Opd, abs: bool, neg: bool) -> Opd {
        let a = self.i_absolute_if(a, abs);
        self.i_negate_if(a, neg)
    }

    pub fn i_absolute_if(&mut self, a: Opd, abs: bool) -> Opd {
        if abs { self.i_absolute(a) } else { a }
    }

    pub fn i_negate_if(&mut self, a: Opd, neg: bool) -> Opd {
        if neg { self.i_negate(a) } else { a }
    }

    /// `PackDouble2x32(double)`.
    pub fn pack_double2x32_f(&mut self, value: f64) -> Opd {
        let bits = value.to_bits() as i64;
        let lo = self.ir.const_(bits as i32);
        let hi = self.ir.const_((bits >> 32) as i32);
        let dest = self.ir.local();
        self.add_op(
            Inst::PackDouble2x32,
            StorageKind::None,
            Some(dest),
            &[lo, hi],
        );
        dest
    }

    fn unpack_indexed(&mut self, inst: Inst, a: Opd, index: i32) -> Opd {
        let dest = self.ir.local();
        let n = self.ir.new_op_multi(inst, index, &[Some(dest)], &[a]);
        self.add_node(n);
        dest
    }

    pub fn unpack_double2x32_high(&mut self, a: Opd) -> Opd {
        self.unpack_indexed(Inst::UnpackDouble2x32, a, 1)
    }

    pub fn unpack_double2x32_low(&mut self, a: Opd) -> Opd {
        self.unpack_indexed(Inst::UnpackDouble2x32, a, 0)
    }

    pub fn unpack_half2x16_high(&mut self, a: Opd) -> Opd {
        self.unpack_indexed(Inst::UnpackHalf2x16, a, 1)
    }

    pub fn unpack_half2x16_low(&mut self, a: Opd) -> Opd {
        self.unpack_indexed(Inst::UnpackHalf2x16, a, 0)
    }

    // ---- IoVariable overloads of Load and Store (a null primVertex / invocationId is `None`) ----

    pub fn load_io(&mut self, kind: StorageKind, io: IoVariable, prim_vertex: Option<Opd>) -> Opd {
        match prim_vertex {
            Some(p) => self.load_b1(kind, io.0 as i32, p),
            None => self.load_b0(kind, io.0 as i32),
        }
    }

    pub fn load_io_e(
        &mut self,
        kind: StorageKind,
        io: IoVariable,
        prim_vertex: Option<Opd>,
        elem: Opd,
    ) -> Opd {
        match prim_vertex {
            Some(p) => self.load_b2(kind, io.0 as i32, p, elem),
            None => self.load_b1(kind, io.0 as i32, elem),
        }
    }

    pub fn load_io_a(
        &mut self,
        kind: StorageKind,
        io: IoVariable,
        prim_vertex: Option<Opd>,
        array: Opd,
        elem: Opd,
    ) -> Opd {
        match prim_vertex {
            Some(p) => self.load_b3(kind, io.0 as i32, p, array, elem),
            None => self.load_b2(kind, io.0 as i32, array, elem),
        }
    }

    /// `Store(kind, io, invocationId, rest...)` with an explicit operand list after the variable and the invocation.
    pub fn store_io_srcs_pub(
        &mut self,
        kind: StorageKind,
        io: IoVariable,
        invocation: Option<Opd>,
        rest: &[Opd],
    ) {
        self.store_io_srcs(kind, io, invocation, rest);
    }

    fn store_io_srcs(
        &mut self,
        kind: StorageKind,
        io: IoVariable,
        invocation: Option<Opd>,
        rest: &[Opd],
    ) {
        let c = self.ir.const_(io.0 as i32);
        let mut src = vec![c];
        src.extend(invocation);
        src.extend_from_slice(rest);
        self.add_op(Inst::Store, kind, None, &src);
    }

    pub fn store_io(
        &mut self,
        kind: StorageKind,
        io: IoVariable,
        invocation: Option<Opd>,
        value: Opd,
    ) {
        self.store_io_srcs(kind, io, invocation, &[value]);
    }

    pub fn store_io_e(
        &mut self,
        kind: StorageKind,
        io: IoVariable,
        invocation: Option<Opd>,
        elem: Opd,
        value: Opd,
    ) {
        self.store_io_srcs(kind, io, invocation, &[elem, value]);
    }

    pub fn store_io_a(
        &mut self,
        kind: StorageKind,
        io: IoVariable,
        invocation: Option<Opd>,
        array: Opd,
        elem: Opd,
        value: Opd,
    ) {
        self.store_io_srcs(kind, io, invocation, &[array, elem, value]);
    }

    // ---- texture and image operations ----

    #[allow(clippy::too_many_arguments)]
    fn tex_op(
        &mut self,
        inst: Inst,
        ty: SamplerType,
        format: usize,
        flags: u32,
        sb: SetBinding,
        comp: i32,
        dests: &[Option<Opd>],
        sources: &[Opd],
    ) {
        let n = self.ir.new_tex_op(
            inst, ty, format, flags, sb.set, sb.binding, comp, dests, sources,
        );
        self.add_node(n);
    }

    pub fn image_atomic(
        &mut self,
        ty: SamplerType,
        format: usize,
        flags: u32,
        sb: SetBinding,
        sources: &[Opd],
    ) -> Opd {
        let dest = self.ir.local();
        self.tex_op(
            Inst::ImageAtomic,
            ty,
            format,
            flags,
            sb,
            0,
            &[Some(dest)],
            sources,
        );
        dest
    }

    #[allow(clippy::too_many_arguments)]
    pub fn image_load(
        &mut self,
        ty: SamplerType,
        format: usize,
        flags: u32,
        sb: SetBinding,
        comp_mask: i32,
        dests: &[Option<Opd>],
        sources: &[Opd],
    ) {
        self.tex_op(
            Inst::ImageLoad,
            ty,
            format,
            flags,
            sb,
            comp_mask,
            dests,
            sources,
        );
    }

    pub fn image_store(
        &mut self,
        ty: SamplerType,
        format: usize,
        flags: u32,
        sb: SetBinding,
        sources: &[Opd],
    ) {
        self.tex_op(Inst::ImageStore, ty, format, flags, sb, 0, &[], sources);
    }

    pub fn lod(
        &mut self,
        ty: SamplerType,
        flags: u32,
        sb: SetBinding,
        comp_index: i32,
        sources: &[Opd],
    ) -> Opd {
        let dest = self.ir.local();
        self.tex_op(
            Inst::Lod,
            ty,
            0,
            flags,
            sb,
            comp_index,
            &[Some(dest)],
            sources,
        );
        dest
    }

    pub fn texture_sample(
        &mut self,
        ty: SamplerType,
        flags: u32,
        sb: SetBinding,
        comp_mask: i32,
        dests: &[Option<Opd>],
        sources: &[Opd],
    ) {
        self.tex_op(
            Inst::TextureSample,
            ty,
            0,
            flags,
            sb,
            comp_mask,
            dests,
            sources,
        );
    }

    pub fn texture_query_samples(
        &mut self,
        ty: SamplerType,
        flags: u32,
        sb: SetBinding,
        sources: &[Opd],
    ) -> Opd {
        let dest = self.ir.local();
        self.tex_op(
            Inst::TextureQuerySamples,
            ty,
            0,
            flags,
            sb,
            0,
            &[Some(dest)],
            sources,
        );
        dest
    }

    pub fn texture_query_size(
        &mut self,
        ty: SamplerType,
        flags: u32,
        sb: SetBinding,
        comp_index: i32,
        sources: &[Opd],
    ) -> Opd {
        let dest = self.ir.local();
        self.tex_op(
            Inst::TextureQuerySize,
            ty,
            0,
            flags,
            sb,
            comp_index,
            &[Some(dest)],
            sources,
        );
        dest
    }

    // ---- warp operations with a second (predicate) result ----

    fn shuffle2(&mut self, inst: Inst, a: Opd, b: Opd, c: Opd) -> (Opd, Opd) {
        let d0 = self.ir.local();
        let d1 = self.ir.local();
        self.add2(inst, d0, d1, &[a, b, c])
    }

    pub fn shuffle_3(&mut self, a: Opd, b: Opd, c: Opd) -> (Opd, Opd) {
        self.shuffle2(Inst::Shuffle, a, b, c)
    }

    pub fn shuffle_down_3(&mut self, a: Opd, b: Opd, c: Opd) -> (Opd, Opd) {
        self.shuffle2(Inst::ShuffleDown, a, b, c)
    }

    pub fn shuffle_up_3(&mut self, a: Opd, b: Opd, c: Opd) -> (Opd, Opd) {
        self.shuffle2(Inst::ShuffleUp, a, b, c)
    }

    pub fn shuffle_xor_3(&mut self, a: Opd, b: Opd, c: Opd) -> (Opd, Opd) {
        self.shuffle2(Inst::ShuffleXor, a, b, c)
    }
}

/// Texture flag helper for callers that combine flags.
pub const fn flags(a: u32, b: u32) -> u32 {
    a | b
}

#[allow(dead_code)]
const _USES: u32 = texflags::NONE;
