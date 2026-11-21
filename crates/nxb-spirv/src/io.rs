//! A pass over naga's IR of an entry point: array varyings.
//!
//! naga's SPIR-V reader wraps the shader's `main` in an entry function. That function stores each argument into a private
//! global, calls `main`, then loads the outputs and returns them, as one value or as a struct built with `Compose`. The pass
//! works on that wrapper only, so the shader body stays as naga read it. It only appends expressions, because an expression
//! can only refer to earlier ones.

use naga::{
    ArraySize, Binding, Block, Expression, FunctionArgument, FunctionResult, Handle, Interpolation,
    Module, Sampling, ScalarKind, Statement, StructMember, Type, TypeInner, UniqueArena,
    proc::Alignment, proc::Layouter,
};

/// The element type and the length of an array of scalars or vectors.
fn array_varying(types: &UniqueArena<Type>, ty: Handle<Type>) -> Option<(Handle<Type>, u32)> {
    match types[ty].inner {
        TypeInner::Array {
            base,
            size: ArraySize::Constant(n),
            ..
        } if matches!(
            types[base].inner,
            TypeInner::Scalar(_) | TypeInner::Vector { .. }
        ) =>
        {
            Some((base, n.get()))
        }
        _ => None,
    }
}

/// The binding of element `add` of an array varying with element type `elem`. naga's SPIR-V reader gives a float varying
/// its default interpolation only when it is not an array, so a float element without one gets perspective at the centre,
/// the default of SPIR-V.
fn at_location(b: &Binding, add: u32, elem: &TypeInner) -> Binding {
    match *b {
        Binding::Location {
            location,
            mut interpolation,
            mut sampling,
            blend_src,
            per_primitive,
        } => {
            if interpolation.is_none() && elem.scalar_kind() == Some(ScalarKind::Float) {
                interpolation = Some(Interpolation::Perspective);
                sampling = Some(Sampling::Center);
            }
            Binding::Location {
                location: location + add,
                interpolation,
                sampling,
                blend_src,
                per_primitive,
            }
        }
        ref other => other.clone(),
    }
}

fn indexed(name: &Option<String>, i: u32) -> Option<String> {
    name.as_ref().map(|n| format!("{n}_{i}"))
}

/// The value of the wrapper's final `return`.
fn returned(body: &Block) -> Option<Handle<Expression>> {
    match body.last() {
        Some(&Statement::Return { value }) => value,
        _ => None,
    }
}

/// Puts `Emit(range)` before the final `return` and makes it return `value`.
fn finish_return(body: &mut Block, emit: naga::Range<Expression>, value: Handle<Expression>) {
    let old = std::mem::take(body);
    let n = old.len();
    for (k, (stmt, span)) in old.span_into_iter().enumerate() {
        if k + 1 == n {
            body.push(Statement::Emit(emit.clone()), span);
            body.push(Statement::Return { value: Some(value) }, span);
        } else {
            body.push(stmt, span);
        }
    }
}

/// Splits each array of scalars or vectors among the inputs and outputs into one varying per element, at consecutive
/// locations. Metal has no arrays in a stage's inputs or outputs. Element `i` of a varying `name` is named `name_i`.
pub(crate) fn flatten_varyings(m: &mut Module) -> Result<(), String> {
    let mut layouter = Layouter::default();
    layouter
        .update(m.to_ctx())
        .map_err(|e| format!("layout: {e}"))?;
    for ep in &mut m.entry_points {
        let f = &mut ep.function;
        // inputs
        let n0 = f.arguments.len();
        let mut splits: Vec<(u32, Vec<u32>)> = Vec::new();
        for k in 0..n0 {
            let a = f.arguments[k].clone();
            let (Some(b @ Binding::Location { .. }), Some((base, n))) =
                (&a.binding, array_varying(&m.types, a.ty))
            else {
                continue;
            };
            let elem = &m.types[base].inner;
            f.arguments[k] = FunctionArgument {
                name: indexed(&a.name, 0),
                ty: base,
                binding: Some(at_location(b, 0, elem)),
            };
            let mut idx = vec![k as u32];
            for i in 1..n {
                idx.push(f.arguments.len() as u32);
                f.arguments.push(FunctionArgument {
                    name: indexed(&a.name, i),
                    ty: base,
                    binding: Some(at_location(b, i, elem)),
                });
            }
            splits.push((k as u32, idx));
        }
        if !splits.is_empty() {
            let old = std::mem::take(&mut f.body);
            let mut done = 0;
            for (stmt, span) in old.span_into_iter() {
                let split = match stmt {
                    Statement::Store { pointer, value } => match f.expressions[value] {
                        Expression::FunctionArgument(k) => splits
                            .iter()
                            .find(|s| s.0 == k)
                            .map(|s| (pointer, value, s.1.clone())),
                        _ => None,
                    },
                    _ => None,
                };
                let Some((pointer, value, idx)) = split else {
                    f.body.push(stmt, span);
                    continue;
                };
                done += 1;
                let values: Vec<_> = idx
                    .iter()
                    .enumerate()
                    .map(|(i, &a)| {
                        if i == 0 {
                            value
                        } else {
                            f.expressions.append(Expression::FunctionArgument(a), span)
                        }
                    })
                    .collect();
                let start = f.expressions.len();
                let ptrs: Vec<_> = (0..idx.len() as u32)
                    .map(|index| {
                        f.expressions.append(
                            Expression::AccessIndex {
                                base: pointer,
                                index,
                            },
                            span,
                        )
                    })
                    .collect();
                f.body
                    .push(Statement::Emit(f.expressions.range_from(start)), span);
                for (p, v) in ptrs.into_iter().zip(values) {
                    f.body.push(
                        Statement::Store {
                            pointer: p,
                            value: v,
                        },
                        span,
                    );
                }
            }
            if done != splits.len() {
                return Err("an array input that is not stored straight from its argument".into());
            }
        }
        // outputs
        let Some(res) = f.result.clone() else {
            continue;
        };
        let Some(ret) = returned(&f.body) else {
            continue;
        };
        let (members, comps) = match (&res.binding, &m.types[res.ty].inner) {
            (None, TypeInner::Struct { members, .. }) => match f.expressions[ret] {
                Expression::Compose { ref components, .. } => (members.clone(), components.clone()),
                _ => return Err("the entry point returns a struct that is not composed".into()),
            },
            (Some(b), _) => (
                vec![StructMember {
                    name: None,
                    ty: res.ty,
                    binding: Some(b.clone()),
                    offset: 0,
                }],
                vec![ret],
            ),
            _ => continue,
        };
        let is_split = |mb: &StructMember| {
            matches!(mb.binding, Some(Binding::Location { .. }))
                && array_varying(&m.types, mb.ty).is_some()
        };
        if !members.iter().any(is_split) {
            continue;
        }
        let span = f.expressions.get_span(ret);
        let start = f.expressions.len();
        let (mut nm, mut nc) = (Vec::new(), Vec::new());
        for (mb, c) in members.iter().zip(comps) {
            match (is_split(mb), &mb.binding, array_varying(&m.types, mb.ty)) {
                (true, Some(b), Some((base, n))) => {
                    for i in 0..n {
                        let binding = Some(at_location(b, i, &m.types[base].inner));
                        nm.push(StructMember {
                            name: indexed(&mb.name, i),
                            ty: base,
                            binding,
                            offset: 0,
                        });
                        nc.push(
                            f.expressions
                                .append(Expression::AccessIndex { base: c, index: i }, span),
                        );
                    }
                }
                _ => {
                    nm.push(mb.clone());
                    nc.push(c);
                }
            }
        }
        let (mut off, mut align) = (0, Alignment::ONE);
        for mb in &mut nm {
            let l = layouter[mb.ty];
            off = l.alignment.round_up(off);
            mb.offset = off;
            off += l.size;
            align = align.max(l.alignment);
        }
        let ty = m.types.insert(
            Type {
                name: None,
                inner: TypeInner::Struct {
                    members: nm,
                    span: align.round_up(off),
                },
            },
            span,
        );
        let value = f
            .expressions
            .append(Expression::Compose { ty, components: nc }, span);
        let emit = f.expressions.range_from(start);
        finish_return(&mut f.body, emit, value);
        f.result = Some(FunctionResult { ty, binding: None });
    }
    Ok(())
}

#[cfg(test)]
#[path = "io_tests.rs"]
mod tests;
