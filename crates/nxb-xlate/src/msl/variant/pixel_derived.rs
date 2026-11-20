//! The derived result entries of a pixel shader: `main0_u`, `main0_m` and `main0_d`.
//!
//! `main0_u` returns every float colour as the unsigned integer vector of the same bits, for a pipeline whose colour
//! attachments are all unsigned integers. `main0_m` does that for the colours the shader writes from integer bits
//! only, for a pipeline that mixes both. `main0_d` moves colour 1 to the second source of colour 0.
//!
//! Which colours hold integer bits is read from the structured program: the kind of the value each store writes.

use super::pixel::Derived;
use crate::ir::agg::{AggType, dest_var_type};
use crate::ir::{Inst, IoVariable, OperandType, StorageKind};
use crate::msl::io::{FragOut, Plan};
use crate::msl::usage::const_of;
use crate::structured::StructInfo;
use crate::structured::ast::{A, Kind};
use crate::structured::walk::for_each_node;
use std::collections::{BTreeMap, BTreeSet};

/// The largest bit pattern that counts as an integer constant: a float below the normal range.
const INT_CONST_LIMIT: i32 = 0x80_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind2 {
    Int,
    Float,
}

type Kinds = BTreeSet<Kind2>;

/// One store to a colour output.
struct Store {
    color: u32,
    value: A,
}

/// What the stores of a program write.
struct Scan<'a> {
    info: &'a StructInfo,
    defs: BTreeMap<A, Vec<A>>,
    stores: Vec<Store>,
    flat: BTreeSet<i32>,
}

fn is_float(t: AggType) -> bool {
    t.0 & AggType::ELEMENT_TYPE_MASK == AggType::FP32.0
}

impl<'a> Scan<'a> {
    fn new(info: &'a StructInfo, plan: &Plan) -> Scan<'a> {
        let mut s = Scan {
            info,
            defs: BTreeMap::new(),
            stores: Vec::new(),
            flat: plan
                .varyings
                .iter()
                .filter(|v| v.interpolation == "flat")
                .map(|v| v.location as i32)
                .collect(),
        };
        let mut defs: BTreeMap<A, Vec<A>> = BTreeMap::new();
        let mut stores = Vec::new();
        for_each_node(info, &mut |_, k| match k {
            Kind::Assign { dest, src } => defs.entry(*dest).or_default().push(*src),
            Kind::Operation(o)
                if Inst(o.inst.0 & Inst::MASK) == Inst::Store
                    && o.storage_kind == StorageKind::Output =>
            {
                let at = |i: usize| o.sources.get(i).and_then(|a| const_of(&info.ast, *a));
                if at(0) == Some(IoVariable::FragmentOutputColor.0 as i32)
                    && let (Some(c), Some(v)) = (at(1), o.sources.last())
                    && let Ok(color) = u32::try_from(c)
                {
                    stores.push(Store { color, value: *v });
                }
            }
            _ => {}
        });
        s.defs = defs;
        s.stores = stores;
        s
    }

    /// The type a node has before any bit cast: `None` for a constant or an undefined value.
    fn natural(&self, a: A) -> Option<AggType> {
        match self.info.ast.kind(a) {
            Kind::Operand {
                ty: OperandType::LocalVariable | OperandType::Argument,
                var_type,
                ..
            } => Some(*var_type),
            Kind::Operation(o) if Inst(o.inst.0 & Inst::MASK) != Inst::Load => {
                dest_var_type(o.inst).ok()
            }
            _ => None,
        }
    }

    /// The kinds of a value read as integer bits.
    fn kinds(&self, a: A, seen: &mut BTreeSet<A>) -> Kinds {
        let mut out = Kinds::new();
        match self.info.ast.kind(a).clone() {
            Kind::Operand {
                ty: OperandType::Constant,
                value,
                ..
            } => {
                if value != 0 && value.unsigned_abs() < INT_CONST_LIMIT as u32 {
                    out.insert(Kind2::Int);
                }
            }
            Kind::Operand {
                ty: OperandType::LocalVariable,
                var_type,
                ..
            } => {
                if is_float(var_type) {
                    out.insert(Kind2::Float);
                } else if seen.insert(a) {
                    for d in self.defs.get(&a).into_iter().flatten() {
                        out.extend(self.kinds(*d, seen));
                    }
                }
            }
            Kind::Operation(o) => {
                let inst = Inst(o.inst.0 & Inst::MASK);
                let src = |i: usize| o.sources.get(i).copied();
                match inst {
                    Inst::ConditionalSelect => {
                        for i in [1, 2] {
                            if let Some(s) = src(i) {
                                out.extend(self.kinds(s, seen));
                            }
                        }
                    }
                    Inst::Copy => {
                        if let Some(s) = src(0) {
                            out.extend(self.kinds(s, seen));
                        }
                    }
                    Inst::Load => {
                        if o.storage_kind == StorageKind::Input {
                            out.insert(Kind2::Int);
                        }
                    }
                    _ if o.tex.is_some() => {}
                    _ => match dest_var_type(o.inst) {
                        Ok(t) if is_float(t) => {
                            out.insert(Kind2::Float);
                        }
                        Ok(t) if t != AggType::INVALID && t != AggType::BOOL => {
                            out.insert(Kind2::Int);
                        }
                        _ => {}
                    },
                }
            }
            _ => {}
        }
        out
    }

    /// The kinds of what a store writes to a float colour.
    fn store_kinds(&self, a: A) -> Kinds {
        match self.natural(a) {
            Some(t) if is_float(t) => Kinds::from([Kind2::Float]),
            _ => self.kinds(a, &mut BTreeSet::new()),
        }
    }

    /// Whether a store writes a value of another type than float, which a bit cast turns into the colour.
    fn is_int_source(&self, a: A) -> bool {
        self.natural(a).is_some_and(|t| !is_float(t))
    }

    fn constant(&self, a: A) -> bool {
        matches!(
            self.info.ast.kind(a),
            Kind::Operand {
                ty: OperandType::Constant,
                ..
            }
        )
    }

    /// A flat input, or a local or copy that holds one.
    fn passed(&self, a: A, seen: &mut BTreeSet<A>) -> bool {
        match self.info.ast.kind(a) {
            Kind::Operand {
                ty: OperandType::LocalVariable,
                ..
            } => {
                let d = self.defs.get(&a).map_or(&[][..], Vec::as_slice);
                seen.insert(a) && !d.is_empty() && d.iter().all(|x| self.passed(*x, seen))
            }
            Kind::Operation(o) => match Inst(o.inst.0 & Inst::MASK) {
                Inst::Copy => o.sources.first().is_some_and(|s| self.passed(*s, seen)),
                Inst::Load => {
                    let at = |i: usize| o.sources.get(i).and_then(|a| const_of(&self.info.ast, *a));
                    o.storage_kind == StorageKind::Input
                        && at(0) == Some(IoVariable::UserDefined.0 as i32)
                        && at(1).is_some_and(|l| self.flat.contains(&l))
                }
                _ => false,
            },
            _ => false,
        }
    }
}

fn uty(ty: &str) -> String {
    format!("u{}", ty.replace("float", "int"))
}

/// The colour outputs that are floats, with their struct members.
fn float_colors(plan: &Plan) -> Vec<&FragOut> {
    plan.frag_outs
        .iter()
        .filter(|m| m.color.is_some() && m.ty.starts_with("float"))
        .collect()
}

/// The colours written from integer bits: those that only hold integer kinds, or else those that hold any.
fn int_colors(s: &Scan<'_>, cols: &[&FragOut]) -> BTreeSet<u32> {
    let mut found: Vec<(u32, Kinds)> = Vec::new();
    for c in cols {
        let n = c.color.unwrap_or(0);
        let mut k = Kinds::new();
        for st in s.stores.iter().filter(|st| st.color == n) {
            k.extend(s.store_kinds(st.value));
        }
        found.push((n, k));
    }
    let only: BTreeSet<u32> = found
        .iter()
        .filter(|(_, k)| k.len() == 1 && k.contains(&Kind2::Int))
        .map(|(n, _)| *n)
        .collect();
    if !only.is_empty() {
        return only;
    }
    found
        .into_iter()
        .filter(|(_, k)| k.contains(&Kind2::Int))
        .map(|(n, _)| n)
        .collect()
}

fn convert(m: &FragOut, as_uint: bool) -> (FragOut, String) {
    if as_uint {
        let t = uty(&m.ty);
        let e = format!("as_type<{t}>(dm_r.{})", m.name);
        (FragOut { ty: t, ..m.clone() }, e)
    } else {
        (m.clone(), format!("dm_r.{}", m.name))
    }
}

/// The derived entries of a pixel shader.
pub fn derived(info: &StructInfo, plan: &Plan, dual: bool) -> Vec<Derived> {
    let mut out = Vec::new();
    let cols = float_colors(plan);
    if !cols.is_empty() {
        let s = Scan::new(info, plan);
        let on_cols: Vec<&Store> = s
            .stores
            .iter()
            .filter(|st| cols.iter().any(|c| c.color == Some(st.color)))
            .collect();
        let ints = int_colors(&s, &cols);
        let from_int = cols.iter().any(|c| {
            s.stores
                .iter()
                .any(|st| Some(st.color) == c.color && s.is_int_source(st.value))
        });
        let all_const = !on_cols.is_empty() && on_cols.iter().all(|st| s.constant(st.value));
        let all_passed = !on_cols.is_empty()
            && on_cols
                .iter()
                .all(|st| s.passed(st.value, &mut BTreeSet::new()));
        if from_int || !ints.is_empty() || all_const || all_passed {
            let is_col = |m: &FragOut| cols.iter().any(|c| c.name == m.name);
            out.push(Derived {
                name: "main0_u".into(),
                members: plan
                    .frag_outs
                    .iter()
                    .map(|m| convert(m, is_col(m)))
                    .collect(),
                uint_mask: None,
            });
            let as_int: BTreeSet<u32> = if ints.is_empty() {
                cols.iter()
                    .filter_map(|c| c.color)
                    .filter(|n| {
                        s.stores
                            .iter()
                            .any(|st| st.color == *n && s.is_int_source(st.value))
                    })
                    .collect()
            } else {
                ints
            };
            if !as_int.is_empty() && cols.iter().any(|c| !as_int.contains(&c.color.unwrap_or(0))) {
                let is_int =
                    |m: &FragOut| is_col(m) && m.color.is_some_and(|n| as_int.contains(&n));
                out.push(Derived {
                    name: "main0_m".into(),
                    members: plan
                        .frag_outs
                        .iter()
                        .map(|m| convert(m, is_int(m)))
                        .collect(),
                    uint_mask: Some(as_int.iter().fold(0u32, |a, n| a | 1u32 << n)),
                });
            }
        }
    }
    if dual {
        let mut attrs: Vec<&str> = plan
            .frag_outs
            .iter()
            .filter(|m| m.attr.starts_with("color("))
            .map(|m| m.attr.as_str())
            .collect();
        attrs.sort_unstable();
        if attrs == ["color(0)", "color(1)"] {
            let members = plan
                .frag_outs
                .iter()
                .map(|m| {
                    let attr = if m.attr == "color(1)" {
                        "color(0), index(1)".to_string()
                    } else {
                        m.attr.clone()
                    };
                    (FragOut { attr, ..m.clone() }, format!("dm_r.{}", m.name))
                })
                .collect();
            out.push(Derived {
                name: "main0_d".into(),
                members,
                uint_mask: None,
            });
        }
    }
    out
}
