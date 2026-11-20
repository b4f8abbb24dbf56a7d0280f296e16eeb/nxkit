//! The first invocation's stores of a compute shader, which leave the shader and happen on the host before the dispatch.
//!
//! A kernel that resets a list tests "I am the first invocation of the dispatch" and stores constants into a storage
//! buffer under that test. Every workgroup of an indirect dispatch races to reset the list, so the store moves to the
//! host. The reflection field `first_store` gives `{reg, word, value}` for each store, in order. The test is
//! `-local.x == workgroup.x << log2(local size)`, or the same test on y and x with `workgroup.z == 0` for a kernel with
//! rows. A value is a constant or a local that holds a constant under the test.
//!
//! The program is read as the structured AST. Only the start of the main function counts: plain assignments, blocks that
//! set a local under the test and blocks of stores under it. Any other use of the test leaves the shader as it is.

use super::args::Res;
use super::usage::const_of;
use crate::ir::agg::AggType;
use crate::ir::{Inst, IoVariable, OperandType, StorageKind};
use crate::structured::StructInfo;
use crate::structured::ast::{A, Ast, BlockType, Kind, OpData};
use nxb_pyjson::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The most stores the host makes.
const MAX_STORES: usize = 4;

/// One store the host makes before the dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    /// The `reg` of the storage buffer's binding.
    pub reg: i64,
    /// The word index.
    pub word: u32,
    /// The value.
    pub value: u32,
}

/// What the emitter leaves out and what the host does instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// The blocks of the main function that the emitter does not write.
    pub skip: BTreeSet<A>,
    /// The stores, in order.
    pub stores: Vec<Store>,
}

/// The test of the first invocation as a set of dimensions that are zero, or its negation.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pred {
    Zero(BTreeSet<u32>),
    NotZero(BTreeSet<u32>),
}

/// What the program computes, as far as the test needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Sym {
    Konst(i32),
    /// The value of a local in the first invocation. Other invocations hold something else.
    FirstConst(i32),
    Lid(u32),
    Wg(u32),
    NotLid(u32),
    NegLid(u32),
    WgShl(u32, u32),
    Pred(Pred),
    Unknown,
}

struct Scan<'a> {
    ast: &'a Ast,
    local: [u32; 3],
    env: BTreeMap<A, Sym>,
}

fn flip(p: Pred) -> Pred {
    match p {
        Pred::Zero(s) => Pred::NotZero(s),
        Pred::NotZero(s) => Pred::Zero(s),
    }
}

impl Scan<'_> {
    fn eval(&self, a: A) -> Sym {
        match self.ast.kind(a) {
            Kind::Operand {
                ty: OperandType::Constant,
                value,
                ..
            } => Sym::Konst(*value),
            Kind::Operand {
                ty: OperandType::LocalVariable,
                ..
            } => self.env.get(&a).cloned().unwrap_or(Sym::Unknown),
            Kind::Operation(o) => self.op(o),
            _ => Sym::Unknown,
        }
    }

    fn src(&self, o: &OpData, i: usize) -> Sym {
        o.sources.get(i).map_or(Sym::Unknown, |a| self.eval(*a))
    }

    fn op(&self, o: &OpData) -> Sym {
        let inst = Inst(o.inst.0 & Inst::MASK);
        match inst {
            Inst::Load if o.storage_kind == StorageKind::Input => self.builtin(o),
            Inst::Copy => self.src(o, 0),
            Inst::BitwiseNot => match self.src(o, 0) {
                Sym::Lid(d) => Sym::NotLid(d),
                _ => Sym::Unknown,
            },
            Inst::Negate => match self.src(o, 0) {
                Sym::Lid(d) => Sym::NegLid(d),
                _ => Sym::Unknown,
            },
            Inst::Add => match (self.src(o, 0), self.src(o, 1)) {
                (Sym::NotLid(d), Sym::Konst(1)) | (Sym::Konst(1), Sym::NotLid(d)) => Sym::NegLid(d),
                _ => Sym::Unknown,
            },
            Inst::ShiftLeft => match (self.src(o, 0), self.src(o, 1)) {
                (Sym::Wg(d), Sym::Konst(k)) if (0..32).contains(&k) => Sym::WgShl(d, k as u32),
                _ => Sym::Unknown,
            },
            Inst::CompareEqual | Inst::CompareNotEqual => {
                let p = self.compare(self.src(o, 0), self.src(o, 1));
                match (p, inst == Inst::CompareNotEqual) {
                    (Some(p), false) => Sym::Pred(p),
                    (Some(p), true) => Sym::Pred(flip(p)),
                    _ => Sym::Unknown,
                }
            }
            Inst::LogicalNot => match self.src(o, 0) {
                Sym::Pred(p) => Sym::Pred(flip(p)),
                _ => Sym::Unknown,
            },
            Inst::LogicalAnd => match (self.src(o, 0), self.src(o, 1)) {
                (Sym::Pred(Pred::Zero(a)), Sym::Pred(Pred::Zero(b))) => {
                    Sym::Pred(Pred::Zero(a.union(&b).copied().collect()))
                }
                _ => Sym::Unknown,
            },
            _ => Sym::Unknown,
        }
    }

    fn builtin(&self, o: &OpData) -> Sym {
        let (Some(v), Some(c)) = (
            o.sources.first().and_then(|a| const_of(self.ast, *a)),
            o.sources.get(1).and_then(|a| const_of(self.ast, *a)),
        ) else {
            return Sym::Unknown;
        };
        if o.sources.len() != 2 || !(0..3).contains(&c) {
            return Sym::Unknown;
        }
        let d = c as u32;
        if v as u32 == IoVariable::ThreadId.0 {
            Sym::Lid(d)
        } else if v as u32 == IoVariable::CtaId.0 {
            Sym::Wg(d)
        } else {
            Sym::Unknown
        }
    }

    /// `-local.d == workgroup.d << k` with `1 << k` the local size of d, or `workgroup.z == 0`.
    fn compare(&self, a: Sym, b: Sym) -> Option<Pred> {
        let one = |d: u32| Pred::Zero(BTreeSet::from([d]));
        match (a, b) {
            (Sym::NegLid(d), Sym::WgShl(e, k)) | (Sym::WgShl(e, k), Sym::NegLid(d))
                if d == e && d < 2 && self.local[d as usize] == 1u32.checked_shl(k)? =>
            {
                Some(one(d))
            }
            (Sym::Wg(2), Sym::Konst(0)) | (Sym::Konst(0), Sym::Wg(2)) => Some(one(2)),
            _ => None,
        }
    }
}

/// True when the expression under `a` reads one of the locals.
fn reads(ast: &Ast, a: A, locals: &BTreeSet<A>) -> bool {
    match ast.kind(a) {
        Kind::Operand {
            ty: OperandType::LocalVariable,
            ..
        } => locals.contains(&a),
        Kind::Operation(o) => o.sources.iter().any(|s| reads(ast, *s, locals)),
        _ => false,
    }
}

/// True when a block under `a` that is not skipped branches on one of the locals.
fn branches_on(ast: &Ast, a: A, locals: &BTreeSet<A>, skip: &BTreeSet<A>) -> bool {
    if skip.contains(&a) {
        return false;
    }
    let Kind::Block(b) = ast.kind(a) else {
        return false;
    };
    b.cond.is_some_and(|c| reads(ast, c, locals))
        || ast
            .children(a)
            .into_iter()
            .any(|c| branches_on(ast, c, locals, skip))
}

/// The store of one constant word in a storage buffer, as `(binding, word, value source)`.
fn store_of(ast: &Ast, res: &Res, o: &OpData) -> Option<(i32, u32, A)> {
    if Inst(o.inst.0 & Inst::MASK) != Inst::Store
        || o.storage_kind != StorageKind::StorageBuffer
        || o.sources.len() != 4
    {
        return None;
    }
    let binding = const_of(ast, o.sources[0])?;
    let field = const_of(ast, o.sources[1])?;
    let word = u32::try_from(const_of(ast, o.sources[2])?).ok()?;
    let f = res
        .sbs
        .get(&binding)?
        .def
        .fields
        .get(usize::try_from(field).ok()?)?;
    let plain = f.ty.0 & AggType::ELEMENT_COUNT_MASK == 0;
    (res.sbs.get(&binding)?.def.layout == 1 && plain).then_some((binding, word, o.sources[3]))
}

/// The reflection value of `first_store`.
pub fn value(f: &Found) -> Value {
    Value::List(
        f.stores
            .iter()
            .map(|s| {
                Value::obj([
                    ("reg", Value::Int(s.reg)),
                    ("word", Value::Int(i64::from(s.word))),
                    ("value", Value::Int(i64::from(s.value))),
                ])
            })
            .collect(),
    )
}

/// The first invocation stores of the main function, when it has the exact shape.
pub fn find(info: &StructInfo, res: &Res, local: [u32; 3]) -> Option<Found> {
    if local[2] != 1 {
        return None;
    }
    let need: BTreeSet<u32> = if local[1] == 1 {
        BTreeSet::from([0])
    } else {
        BTreeSet::from([0, 1, 2])
    };
    let ast = &info.ast;
    let mut sc = Scan {
        ast,
        local,
        env: BTreeMap::new(),
    };
    let main = info.functions.first()?.main_block;
    let children = ast.children(main);
    let (mut found, mut guards) = (Found::default(), BTreeSet::new());
    let mut end = children.len();
    for (i, &n) in children.iter().enumerate() {
        match ast.kind(n) {
            Kind::Assign { dest, src } => {
                let v = sc.eval(*src);
                if matches!(&v, Sym::Pred(Pred::Zero(s) | Pred::NotZero(s)) if *s == need) {
                    guards.insert(*dest);
                }
                sc.env.insert(*dest, v);
            }
            Kind::Block(b) if b.ty == BlockType::If => {
                let hit = b.cond.map(|c| sc.eval(c));
                if hit != Some(Sym::Pred(Pred::Zero(need.clone()))) {
                    end = i;
                    break;
                }
                let mut sets = Vec::new();
                let mut stores = Vec::new();
                for c in ast.children(n) {
                    match ast.kind(c) {
                        Kind::Assign { dest, src } => match sc.eval(*src) {
                            Sym::Konst(v) | Sym::FirstConst(v) => sets.push((*dest, v)),
                            _ => return None,
                        },
                        Kind::Operation(o) => {
                            let (binding, word, v) = store_of(ast, res, o)?;
                            let (Sym::Konst(v) | Sym::FirstConst(v)) = sc.eval(v) else {
                                return None;
                            };
                            stores.push((binding, word, v as u32));
                        }
                        _ => return None,
                    }
                }
                if !sets.is_empty() && !stores.is_empty() {
                    return None;
                }
                for (dest, v) in sets {
                    sc.env.insert(dest, Sym::FirstConst(v));
                }
                if !stores.is_empty() {
                    found.skip.insert(n);
                }
                for (binding, word, value) in stores {
                    let name = &res.sbs.get(&binding)?.name;
                    let reg = res
                        .bindings
                        .iter()
                        .find(|b| &b.msl_name == name)
                        .and_then(|b| b.reg)?;
                    found.stores.push(Store { reg, word, value });
                }
            }
            _ => {
                end = i;
                break;
            }
        }
    }
    if found.stores.is_empty() || found.stores.len() > MAX_STORES {
        return None;
    }
    for &n in &children[end..] {
        if branches_on(ast, n, &guards, &found.skip) {
            return None;
        }
    }
    Some(found)
}
