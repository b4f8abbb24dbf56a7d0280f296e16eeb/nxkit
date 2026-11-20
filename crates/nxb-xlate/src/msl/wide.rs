//! Wide loads: neighbouring words of a read-only storage buffer, read whole.
//!
//! The front end reads a 128 bit load as four word loads. A run of two to four loads of consecutive words that start at
//! a multiple of four words from their shared base becomes one `packed_uintN` load, and the loads are lanes of it. The
//! shader reads exactly the words it read before. The only change is the number of memory transactions.
//!
//! The pass reads the structured AST. An index is a constant or a local with one definition, plus constants, with
//! an optional shift right by two. Members of one run are the direct `local = load` statements of one block, so the
//! first member dominates the others, and the buffer is declared `const`, so nothing in the shader writes it.

use super::args::Res;
use crate::ir::agg::AggType;
use crate::ir::{Inst, OperandType, StorageKind};
use crate::structured::StructInfo;
use crate::structured::ast::{A, Ast, Kind, OpData};
use std::collections::{BTreeMap, BTreeSet};

/// What the emitter knows of a load that belongs to a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lane {
    /// The run, numbered in program order.
    pub run: u32,
    /// The position in the run.
    pub lane: u32,
    /// The number of words of the run.
    pub len: u32,
    /// The type of the buffer's words.
    pub ty: AggType,
}

/// What an index is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Base {
    Zero,
    Local(A),
    Arg(i32),
}

/// `base`, shifted right by two when `shr`, plus `off`.
type Index = (Base, bool, i64);

struct Scan<'a> {
    ast: &'a Ast,
    /// The number of definitions of each local and the source of the last one. A local that is the out argument of
    /// a call counts twice.
    defs: BTreeMap<A, (u32, Option<A>)>,
    /// The arguments the program assigns.
    args: BTreeSet<i32>,
}

impl Scan<'_> {
    fn index(&self, a: A, depth: u32) -> Option<Index> {
        if depth > 16 {
            return None;
        }
        match self.ast.kind(a) {
            Kind::Operand {
                ty: OperandType::Constant,
                value,
                ..
            } => Some((Base::Zero, false, i64::from(*value))),
            Kind::Operand {
                ty: OperandType::Argument,
                value,
                ..
            } => (!self.args.contains(value)).then_some((Base::Arg(*value), false, 0)),
            Kind::Operand {
                ty: OperandType::LocalVariable,
                ..
            } => match self.defs.get(&a) {
                Some((1, src)) => Some(src.and_then(|s| self.index(s, depth + 1)).unwrap_or((
                    Base::Local(a),
                    false,
                    0,
                ))),
                _ => None,
            },
            Kind::Operation(o) => self.operation(o, depth),
            _ => None,
        }
    }

    fn operation(&self, o: &OpData, depth: u32) -> Option<Index> {
        let at = |i: usize| o.sources.get(i).copied();
        match Inst(o.inst.0 & Inst::MASK) {
            Inst::Copy => self.index(at(0)?, depth + 1),
            Inst::Add => {
                let k = |i: usize| super::usage::const_of(self.ast, at(i)?);
                let (x, k) = match (k(0), k(1)) {
                    (_, Some(k)) => (at(0)?, k),
                    (Some(k), None) => (at(1)?, k),
                    _ => return None,
                };
                let (b, shr, off) = self.index(x, depth + 1)?;
                Some((b, shr, off + i64::from(k)))
            }
            Inst::ShiftRightU32 | Inst::ShiftRightS32 => {
                if super::usage::const_of(self.ast, at(1)?) != Some(2) {
                    return None;
                }
                let (b, shr, off) = self.index(at(0)?, depth + 1)?;
                match (b, shr) {
                    (Base::Zero, _) => Some((Base::Zero, false, off >> 2)),
                    (_, false) if off % 4 == 0 => Some((b, true, off / 4)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `local = load` of a word of a read-only storage buffer: the load, its buffer, its field, its type and its index.
    fn word_load(&self, res: &Res, stmt: A) -> Option<(A, (i32, i32), AggType, Index)> {
        let Kind::Assign { src, .. } = self.ast.kind(stmt) else {
            return None;
        };
        let Kind::Operation(o) = self.ast.kind(*src) else {
            return None;
        };
        if Inst(o.inst.0 & Inst::MASK) != Inst::Load
            || o.storage_kind != StorageKind::StorageBuffer
            || o.sources.len() != 3
        {
            return None;
        }
        let konst = |i: usize| super::usage::const_of(self.ast, o.sources[i]);
        let (binding, field) = (konst(0)?, konst(1)?);
        let buf = res.sbs.get(&binding)?;
        let f = buf.def.fields.get(usize::try_from(field).ok()?)?;
        let ty = super::types::scalar(f.ty);
        let word = ty == AggType::U32 || ty == AggType::S32 || ty == AggType::FP32;
        let plain = f.ty.0 & AggType::ELEMENT_COUNT_MASK == 0;
        let array = f.ty.0 & AggType::ARRAY.0 != 0 || f.array_length == 0;
        if buf.write || buf.def.layout != 1 || !word || !plain || !array {
            return None;
        }
        Some((*src, (binding, field), ty, self.index(o.sources[2], 0)?))
    }
}

/// Every block under `a`, the block itself first.
fn blocks(ast: &Ast, a: A, out: &mut Vec<A>) {
    if matches!(ast.kind(a), Kind::Block(_)) {
        out.push(a);
        for c in ast.children(a) {
            blocks(ast, c, out);
        }
    }
}

/// The loads of the program that become lanes of a wide load.
pub fn plan(info: &StructInfo, res: &Res, skip: &BTreeSet<A>) -> BTreeMap<A, Lane> {
    let mut sc = Scan {
        ast: &info.ast,
        defs: BTreeMap::new(),
        args: BTreeSet::new(),
    };
    crate::structured::walk::for_each_node(info, &mut |_, k| match k {
        Kind::Assign { dest, src } => match sc.ast.kind(*dest) {
            Kind::Operand {
                ty: OperandType::LocalVariable,
                ..
            } => {
                let e = sc.defs.entry(*dest).or_insert((0, None));
                *e = (e.0 + 1, Some(*src));
            }
            Kind::Operand {
                ty: OperandType::Argument,
                value,
                ..
            } => {
                sc.args.insert(*value);
            }
            _ => {}
        },
        Kind::Operation(o) if Inst(o.inst.0 & Inst::MASK) == Inst::Call => {
            for s in &o.sources {
                if matches!(
                    sc.ast.kind(*s),
                    Kind::Operand {
                        ty: OperandType::LocalVariable,
                        ..
                    }
                ) {
                    sc.defs.entry(*s).or_insert((0, None)).0 += 2;
                }
            }
        }
        _ => {}
    });
    let mut all = Vec::new();
    for f in &info.functions {
        blocks(&info.ast, f.main_block, &mut all);
    }
    let mut out = BTreeMap::new();
    let mut runs = 0;
    for b in all.into_iter().filter(|b| !skip.contains(b)) {
        type Key = ((i32, i32), Base, bool);
        let mut words: BTreeMap<Key, BTreeMap<i64, (usize, A, AggType)>> = BTreeMap::new();
        for (i, stmt) in info.ast.children(b).into_iter().enumerate() {
            if let Some((load, buf, ty, (base, shr, off))) = sc.word_load(res, stmt) {
                words
                    .entry((buf, base, shr))
                    .or_default()
                    .entry(off)
                    .or_insert((i, load, ty));
            }
        }
        for by_off in words.values() {
            let quads: BTreeSet<i64> = by_off.keys().map(|o| o - o.rem_euclid(4)).collect();
            for q in quads {
                let mut run = Vec::new();
                while run.len() < 4
                    && let Some(w) = by_off.get(&(q + run.len() as i64))
                {
                    run.push(*w);
                }
                if run.len() < 2 || run.iter().any(|w| w.0 < run[0].0) {
                    continue;
                }
                for (lane, (_, load, ty)) in run.iter().enumerate() {
                    out.insert(
                        *load,
                        Lane {
                            run: runs,
                            lane: lane as u32,
                            len: run.len() as u32,
                            ty: *ty,
                        },
                    );
                }
                runs += 1;
            }
        }
    }
    out
}
