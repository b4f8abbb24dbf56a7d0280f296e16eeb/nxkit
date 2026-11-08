//! The resources and features a structured program uses, found by one walk over its nodes.

use super::walk::for_each_node;
use crate::ir::{Inst, IoVariable, OperandType, StorageKind};
use crate::structured::StructInfo;
use crate::structured::ast::{A, Ast, Kind};
use crate::types::SamplerType;
use std::collections::{BTreeMap, BTreeSet};

/// `TextureFlags.IntCoords`.
const INT_COORDS: u32 = 1 << 3;

/// How an image is accessed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageUse {
    /// Loads.
    pub read: bool,
    /// Stores.
    pub write: bool,
    /// Atomics.
    pub atomic: bool,
}

/// What a program uses.
#[derive(Debug, Default)]
pub struct Usage {
    /// Constant buffer bindings that are read.
    pub cb: BTreeSet<i32>,
    /// Storage buffer bindings and whether they are written.
    pub sb: BTreeMap<i32, bool>,
    /// Texture `(set, binding)` pairs that are sampled or queried.
    pub tex: BTreeSet<(i32, i32)>,
    /// Sampler `(set, binding)` pairs of separate samplers.
    pub smp: BTreeSet<(i32, i32)>,
    /// Images and their access.
    pub img: BTreeMap<(i32, i32), ImageUse>,
    /// Textures read with `get_width` (a size query).
    pub sized: BTreeSet<(i32, i32)>,
    /// Samplers used by a plain `.sample(`, which the border emulation wraps: `(set, binding, separate)`. A combined
    /// sampler is named by its texture.
    pub plain_sample: BTreeSet<(i32, i32, bool)>,
    /// Combined textures that need their sampler: `(set, binding)`.
    pub sampled: BTreeSet<(i32, i32)>,
    /// The program has a threadgroup barrier.
    pub barrier: bool,
    /// The program uses a lane id (the subgroup ops).
    pub lane_id: bool,
    /// Fragment shader writes a depth.
    pub frag_depth: bool,
    /// The program discards.
    pub discard: bool,
    /// The program has a fragment shader interlock region.
    pub interlock: bool,
    /// The program has a loop, a switch lowering or a call.
    pub calls: bool,
    /// Constant buffer bindings read at the constant vec4 index 100, component 1 (the base instance).
    pub base_inst: BTreeSet<i32>,
    /// The program reads `render_scale[0]` of the support buffer (the pixel position is divided by the pass scale).
    pub pass_scale: bool,
}

/// The constant operand `a`, if it is one.
pub fn const_of(ast: &Ast, a: A) -> Option<i32> {
    match ast.kind(a) {
        Kind::Operand {
            ty: OperandType::Constant,
            value,
            ..
        } => Some(*value),
        _ => None,
    }
}

/// Walk the program.
pub fn collect(info: &StructInfo) -> Usage {
    let mut u = Usage::default();
    let ast = &info.ast;
    for_each_node(info, &mut |_, k| {
        let Kind::Operation(o) = k else { return };
        let inst = Inst(o.inst.0 & Inst::MASK);
        let store_like = inst == Inst::Store || super::inst_mem::is_atomic(inst);
        match o.storage_kind {
            StorageKind::ConstantBuffer => {
                if let Some(b) = o.sources.first().and_then(|s| const_of(ast, *s)) {
                    u.cb.insert(b);
                    let at = |i: usize| o.sources.get(i).and_then(|s| const_of(ast, *s));
                    // `support_buffer` is binding 0, `render_scale` its field 5.
                    if b == 0 && o.sources.len() == 3 && at(1) == Some(5) && at(2) == Some(0) {
                        u.pass_scale = true;
                    }
                    if o.sources.len() == 4
                        && inst == Inst::Load
                        && at(2) == Some(100)
                        && at(3) == Some(1)
                    {
                        u.base_inst.insert(b);
                    }
                }
            }
            StorageKind::StorageBuffer => {
                if let Some(b) = o.sources.first().and_then(|s| const_of(ast, *s)) {
                    let w = u.sb.entry(b).or_insert(false);
                    *w |= store_like;
                }
            }
            StorageKind::Output => {
                u.frag_depth |= o.sources.first().and_then(|s| const_of(ast, *s))
                    == Some(IoVariable::FragmentOutputDepth.0 as i32);
            }
            _ => {}
        }
        if let Some(t) = &o.tex {
            match inst {
                Inst::ImageLoad | Inst::ImageStore | Inst::ImageAtomic => {
                    let e = u.img.entry((t.set, t.binding)).or_default();
                    e.read |= inst == Inst::ImageLoad;
                    e.write |= inst == Inst::ImageStore;
                    e.atomic |= inst == Inst::ImageAtomic;
                }
                _ => {
                    u.tex.insert((t.set, t.binding));
                    if inst == Inst::TextureQuerySize && o.index != 3 {
                        u.sized.insert((t.set, t.binding));
                    }
                    let fetch = inst == Inst::TextureSample && t.flags & INT_COORDS != 0;
                    if matches!(inst, Inst::TextureSample | Inst::Lod) && !fetch {
                        if t.is_separate() {
                            u.smp.insert((t.sampler_set, t.sampler_binding));
                        } else {
                            u.sampled.insert((t.set, t.binding));
                        }
                    }
                    if inst == Inst::TextureSample && is_plain_sample(t.flags, t.ty) {
                        let (s, b) = if t.is_separate() {
                            (t.sampler_set, t.sampler_binding)
                        } else {
                            (t.set, t.binding)
                        };
                        u.plain_sample.insert((s, b, t.is_separate()));
                    }
                }
            }
        }
        match inst {
            Inst::Barrier | Inst::SubgroupBarrier => u.barrier = true,
            Inst::Discard => u.discard = true,
            Inst::FSIBegin | Inst::FSIEnd => u.interlock = true,
            Inst::Call => u.calls = true,
            Inst::SwizzleAdd => u.lane_id = true,
            _ => {}
        }
    });
    u
}

/// A texture operation that becomes `.sample(` of a colour texture (no gather, no fetch, no depth compare).
pub fn is_plain_sample(flags: u32, ty: SamplerType) -> bool {
    flags & ((1 << 1) | (1 << 3)) == 0
        && ty.0 & (SamplerType::SHADOW | SamplerType::MULTISAMPLE) == 0
        && ty.0 & SamplerType::MASK != 2
}
