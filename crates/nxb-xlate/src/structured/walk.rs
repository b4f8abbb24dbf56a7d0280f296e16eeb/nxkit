//! A walk over the nodes of a structured program that a function reaches.

use crate::structured::StructInfo;
use crate::structured::ast::{A, Kind};

/// Call `f` for every node reachable from the main blocks and locals of the functions, parents before children.
pub fn for_each_node(info: &StructInfo, f: &mut dyn FnMut(A, &Kind)) {
    for func in &info.functions {
        for &l in &func.locals {
            f(l, info.ast.kind(l));
        }
        visit(info, func.main_block, f);
    }
}

pub(crate) fn visit(info: &StructInfo, a: A, f: &mut dyn FnMut(A, &Kind)) {
    let kind = info.ast.kind(a);
    f(a, kind);
    match kind {
        Kind::Block(b) => {
            if let Some(c) = b.cond {
                visit(info, c, f);
            }
            for c in info.ast.children(a) {
                visit(info, c, f);
            }
        }
        Kind::Operation(o) => {
            for &s in &o.sources {
                visit(info, s, f);
            }
        }
        Kind::Assign { dest, src } => {
            visit(info, *dest, f);
            visit(info, *src, f);
        }
        Kind::Operand { .. } | Kind::Comment(_) => {}
    }
}

/// True when a reachable operand or operation is 64 bit floating point.
pub fn uses_float64(info: &StructInfo) -> bool {
    use crate::ir::Inst;
    use crate::ir::agg::AggType;
    let mut found = false;
    for_each_node(info, &mut |_, k| match k {
        Kind::Operand { var_type, .. } => {
            found |= var_type.0 & AggType::ELEMENT_TYPE_MASK == AggType::FP64.0;
        }
        Kind::Operation(o) => found |= o.inst.0 & Inst::FP64 != 0,
        _ => {}
    });
    found
}
