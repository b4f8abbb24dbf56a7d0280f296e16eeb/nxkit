//! A symbolic trace of the stores of a straight-line vertex shader.
//!
//! The facts about the position and the varyings (`facts_vs`) hold when the shader copies attributes and applies
//! affine maps to them. The trace follows the main function from top to bottom and keeps, for every output
//! component, the value of its last store as an expression over attributes, uniforms and constants.

use super::args::{Res, bank_name};
use super::io::Plan;
use super::types::scalar;
use super::usage::const_of;
use crate::ir::OperandType;
use crate::ir::agg::AggType;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::Props;
use crate::structured::StructInfo;
use crate::structured::ast::{A, Kind, OpData};
use std::collections::BTreeMap;
use std::rc::Rc;

/// A value of the shader.
pub type Sym = Rc<Node>;

/// The shape of a value.
#[derive(Debug, PartialEq)]
pub enum Node {
    /// Component `.1` of the attribute at location `.0`.
    Attr(u32, u32),
    /// A uniform word: the slot of the user constant buffer (`vp_c3` is slot 0), the vec4 index and the component.
    Uni([u32; 3]),
    /// A float constant by its bits.
    Konst(u32),
    /// `fma(a, b, c)`.
    Fma(Sym, Sym, Sym),
    /// `a + b`.
    Add(Sym, Sym),
    /// `a - b`.
    Sub(Sym, Sym),
    /// `a * b`.
    Mul(Sym, Sym),
    /// `a / b`.
    Div(Sym, Sym),
    /// `-a`.
    Neg(Sym),
    /// Anything else.
    Other,
}

/// The last stores of the shader by variable, location and component.
pub struct Trace {
    /// The value of the last store to `(variable, location, component)`.
    pub stores: BTreeMap<(u32, i32, i32), Sym>,
}

impl Trace {
    /// The last store to a component of the position.
    pub fn pos(&self, comp: i32) -> Option<&Sym> {
        self.stores.get(&(IoVariable::Position.0, 0, comp))
    }

    /// The last store to a component of the user output at `loc`.
    pub fn user(&self, loc: u32, comp: i32) -> Option<&Sym> {
        self.stores
            .get(&(IoVariable::UserDefined.0, loc as i32, comp))
    }
}

struct Walk<'a> {
    info: &'a StructInfo,
    props: &'a Props,
    plan: &'a Plan,
    res: &'a Res,
    vals: BTreeMap<A, Sym>,
    stores: BTreeMap<(u32, i32, i32), Sym>,
    ok: bool,
}

/// Trace the main function. `None` when it is not straight-line code or writes clip distances.
pub fn trace(info: &StructInfo, props: &Props, plan: &Plan, res: &Res) -> Option<Trace> {
    if props.definitions.oa_indexing {
        return None;
    }
    let main = info.functions.first()?.main_block;
    let mut w = Walk {
        info,
        props,
        plan,
        res,
        vals: BTreeMap::new(),
        stores: BTreeMap::new(),
        ok: true,
    };
    for n in info.ast.children(main) {
        match info.ast.kind(n) {
            Kind::Block(_) => return None,
            Kind::Assign { dest, src } => w.assign(*dest, *src),
            Kind::Operation(o) => match Inst(o.inst.0 & Inst::MASK) {
                Inst::Call => return None,
                Inst::Return => break,
                Inst::Store => w.store(o),
                _ => {}
            },
            _ => {}
        }
    }
    w.ok.then_some(Trace { stores: w.stores })
}

fn other() -> Sym {
    Rc::new(Node::Other)
}

impl Walk<'_> {
    fn assign(&mut self, dest: A, src: A) {
        let Kind::Operand {
            ty: OperandType::LocalVariable,
            var_type,
            ..
        } = self.info.ast.kind(dest)
        else {
            return;
        };
        let v = if *var_type == AggType::FP32 {
            self.eval(src)
        } else {
            other()
        };
        self.vals.insert(dest, v);
    }

    fn store(&mut self, o: &OpData) {
        let k = |i: usize| o.sources.get(i).and_then(|a| const_of(&self.info.ast, *a));
        let Some(var) = k(0).map(|v| IoVariable(v as u32)) else {
            self.ok = false;
            return;
        };
        if o.storage_kind != StorageKind::Output {
            return;
        }
        if var == IoVariable::ClipDistance {
            self.ok = false;
            return;
        }
        let per = usize::from(
            self.props
                .definitions
                .has_per_location_input_or_output(var, true),
        );
        let loc = if per == 1 { k(1) } else { Some(0) };
        let (Some(loc), Some(comp), true) = (loc, k(1 + per), o.sources.len() == 3 + per) else {
            self.ok = false;
            return;
        };
        if var != IoVariable::Position && var != IoVariable::UserDefined {
            return;
        }
        let v = self.eval(o.sources[2 + per]);
        self.stores.insert((var.0, loc, comp), v);
    }

    fn eval(&self, a: A) -> Sym {
        match self.info.ast.kind(a) {
            Kind::Operand {
                ty: OperandType::LocalVariable,
                var_type,
                ..
            } if *var_type == AggType::FP32 => self.vals.get(&a).cloned().unwrap_or_else(other),
            Kind::Operand {
                ty: OperandType::Constant,
                value,
                ..
            } => Rc::new(Node::Konst(*value as u32)),
            Kind::Operation(o) => self.op(o),
            _ => other(),
        }
    }

    fn op(&self, o: &OpData) -> Sym {
        let inst = Inst(o.inst.0 & Inst::MASK);
        let float = o.inst.0 & Inst::FP32 != 0 && o.inst.0 & Inst::FP64 == 0;
        let s = |i: usize| o.sources.get(i).map(|a| self.eval(*a));
        let n = match (inst, float) {
            (Inst::Load, _) => return self.load(o).unwrap_or_else(other),
            (Inst::FusedMultiplyAdd, true) => match (s(0), s(1), s(2)) {
                (Some(a), Some(b), Some(c)) => Node::Fma(a, b, c),
                _ => Node::Other,
            },
            (Inst::Add, true) => bin(s(0), s(1), Node::Add),
            (Inst::Subtract, true) => bin(s(0), s(1), Node::Sub),
            (Inst::Multiply, true) => bin(s(0), s(1), Node::Mul),
            (Inst::Divide, true) => bin(s(0), s(1), Node::Div),
            (Inst::Negate, true) => s(0).map_or(Node::Other, Node::Neg),
            _ => Node::Other,
        };
        Rc::new(n)
    }

    fn load(&self, o: &OpData) -> Option<Sym> {
        let k = |i: usize| o.sources.get(i).and_then(|a| const_of(&self.info.ast, *a));
        match o.storage_kind {
            StorageKind::Input => {
                let loc = u32::try_from(k(1)?).ok()?;
                let float = self
                    .plan
                    .attributes
                    .iter()
                    .any(|(l, t)| *l == loc && t == "float4");
                let comp = u32::try_from(k(2)?).ok()?;
                let ok = o.sources.len() == 3 && k(0) == Some(IoVariable::UserDefined.0 as i32);
                (ok && float).then(|| Rc::new(Node::Attr(loc, comp)))
            }
            StorageKind::ConstantBuffer => {
                let buf = self.res.cbs.get(&k(0)?)?;
                let first = buf.def.fields.first()?;
                let slot = bank_name(&buf.name, 'c')? - 3;
                let (vec, comp) = (k(2)?, k(3)?);
                let ok = o.sources.len() == 4
                    && k(1) == Some(0)
                    && scalar(first.ty) == AggType::FP32
                    && (0..=15).contains(&slot)
                    && (0..=4095).contains(&vec)
                    && (0..=3).contains(&comp);
                ok.then(|| Rc::new(Node::Uni([slot as u32, vec as u32, comp as u32])))
            }
            _ => None,
        }
    }
}

fn bin(a: Option<Sym>, b: Option<Sym>, f: fn(Sym, Sym) -> Node) -> Node {
    match (a, b) {
        (Some(a), Some(b)) => f(a, b),
        _ => Node::Other,
    }
}
