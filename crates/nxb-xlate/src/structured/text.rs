//! Text form of the structured program (the `ast` phase of the probe, `AstText` in `tools/xlate-probe/Probe.cs`).

use super::StructInfo;
use super::ast::*;
use crate::ir::OperandType;
use std::collections::HashMap;
use std::fmt::Write;

fn expr(ast: &Ast, n: Option<A>, ids: &mut HashMap<A, usize>) -> String {
    let Some(n) = n else { return "_".into() };
    match ast.kind(n) {
        Kind::Operand {
            ty,
            var_type,
            value,
        } => match ty {
            OperandType::LocalVariable => {
                let k = ids.len();
                let k = *ids.entry(n).or_insert(k);
                format!("l{}:{}", k, var_type.0)
            }
            OperandType::Constant => format!("#{:x}", *value as u32),
            OperandType::Argument => format!("a{value}"),
            OperandType::Undefined => "undef".into(),
            other => format!("?{other:?}"),
        },
        Kind::Operation(op) => {
            let mut s = format!(
                "({:x} {} {} {}",
                op.inst.0, op.storage_kind.0, op.force_precise as i32, op.index
            );
            if let Some(t) = &op.tex {
                let _ = write!(
                    s,
                    " t={},{},{},{},{},{},{}",
                    t.ty.0, t.format, t.flags, t.set, t.binding, t.sampler_set, t.sampler_binding
                );
            }
            for x in &op.sources {
                s.push(' ');
                s.push_str(&expr(ast, Some(*x), ids));
            }
            s.push(')');
            s
        }
        _ => "?".into(),
    }
}

fn block_text(ast: &Ast, b: A, depth: usize, ids: &mut HashMap<A, usize>, out: &mut String) {
    let ind = " ".repeat(depth);
    for n in ast.children(b) {
        match ast.kind(n) {
            Kind::Block(c) => {
                let cond = match c.cond {
                    None => "_".to_string(),
                    Some(x) => expr(ast, Some(x), ids),
                };
                let _ = writeln!(out, "{ind}blk {:?} {cond} {{", c.ty);
                block_text(ast, n, depth + 1, ids, out);
                let _ = writeln!(out, "{ind}}}");
            }
            Kind::Assign { dest, src } => {
                let d = expr(ast, Some(*dest), ids);
                let s = expr(ast, Some(*src), ids);
                let _ = writeln!(out, "{ind}asg {d} = {s}");
            }
            Kind::Comment(c) => {
                let _ = writeln!(out, "{ind}cm {}", c.replace('\n', " "));
            }
            _ => {
                let e = expr(ast, Some(n), ids);
                let _ = writeln!(out, "{ind}op {e}");
            }
        }
    }
}

/// The whole program as text.
pub fn ast_text(info: &StructInfo) -> String {
    let mut out = String::from("== attempt\n");
    let ast = &info.ast;
    for f in &info.functions {
        let mut ids: HashMap<A, usize> = HashMap::new();
        for l in &f.locals {
            let k = ids.len();
            ids.insert(*l, k);
        }
        let join = |v: &[crate::ir::agg::AggType]| {
            v.iter()
                .map(|x| x.0.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        let _ = writeln!(
            out,
            "func {} ret={} in={} out={}",
            f.name,
            f.return_type.0,
            join(&f.in_arguments),
            join(&f.out_arguments)
        );
        for l in &f.locals {
            if let Kind::Operand { var_type, .. } = ast.kind(*l) {
                let _ = writeln!(out, " local {} {}", ids[l], var_type.0);
            }
        }
        block_text(ast, f.main_block, 1, &mut ids, &mut out);
    }
    out
}
