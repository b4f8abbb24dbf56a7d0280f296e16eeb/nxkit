//! Syntax tree to [`hir`]: scopes, places, conversions, overloads, helper expansion.

use std::collections::HashMap;
use std::rc::Rc;

use msl_parse::ast::{self, Callee, CastKind, ExprKind as A, Init, Item, SwitchEntry, VarScope};
use msl_parse::ty::{arith, ScalarName, Ty};

use crate::hir::{
    Arg, BinOp, CmpOp, ConstArray, Expr, ExprKind, FnConst, Func, Local, LocalId, Module, Param,
    Place, PlaceKind, Query, SampleOpt, Stmt, SwitchArm, TexOp, UnOp,
};
use crate::types::{is_float, Access, Scalar, Space, Tex, TexKind, Type};
use crate::{builtins, conv, Error};

mod pool;

type R<T> = Result<T, Error>;

/// A dm_bs instantiation: argument types, sampling option per extra argument, function.
type BsInstance = (Vec<Type>, Vec<Option<&'static str>>, usize);

/// Analyses an accepted module.
pub fn analyze(m: &ast::Module) -> R<Module> {
    let mut cx = Cx::default();
    for item in &m.items {
        cx.item(item)?;
    }
    Ok(cx.out)
}

#[derive(Default)]
struct Cx {
    out: Module,
    struct_ids: HashMap<Rc<str>, usize>,
    fconst_ids: HashMap<Rc<str>, usize>,
    const_ids: HashMap<Rc<str>, usize>,
    /// Signatures of the lowered functions, to resolve `Callee::User` by pointer.
    sigs: Vec<(Rc<ast::Signature>, usize)>,
    preludes: Vec<&'static str>,
    /// A file-scope `#pragma clang fp contract(off)` came before the current item.
    contract_off: bool,
    // per function
    locals: Vec<Local>,
    scopes: Vec<HashMap<Rc<str>, LocalId>>,
    /// Parameters passed by reference: local -> (space, value type).
    ref_locals: HashMap<LocalId, (Space, Type)>,
    ret: Type,
    /// dm_bs instantiations: argument types -> function.
    bs_inst: Vec<BsInstance>,
    /// dm_pool_mark instantiations: pool pointer type -> function.
    pool_inst: Vec<(Type, usize)>,
}

fn parse_int(text: &str) -> R<i128> {
    let t = text.trim_end_matches(['u', 'U', 'l', 'L']);
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i128::from_str_radix(h, 16)
    } else if t.len() > 1 && t.starts_with('0') {
        i128::from_str_radix(&t[1..], 8)
    } else {
        t.parse::<i128>()
    }
    .map_err(|_| Error::invalid(format!("integer literal {text}")))?;
    Ok(if neg { -v } else { v })
}

fn parse_float(text: &str) -> R<f64> {
    let t = text.trim_end_matches(['f', 'F', 'h', 'H']);
    let v: f32 = t
        .parse()
        .map_err(|_| Error::invalid(format!("float literal {text}")))?;
    Ok(v as f64)
}

impl Cx {
    // ------------------------------------------------------------------ types

    fn ty(&self, t: &Ty) -> R<Type> {
        Ok(match t {
            Ty::Scalar(n) => Type::Scalar(self.scalar(n)?),
            Ty::Vec { base, w, packed } => {
                let s = self.scalar(base)?;
                if *packed {
                    Type::Packed(s, *w)
                } else {
                    Type::Vector(s, *w)
                }
            }
            Ty::Mat { base, cols, rows } => Type::Matrix(*base, *cols, *rows),
            Ty::Struct(name) => match self.struct_ids.get(name) {
                Some(id) => Type::Struct(*id),
                None => return Err(Error::invalid(format!("unknown struct {name}"))),
            },
            Ty::Ptr(sp, inner) => Type::Ptr(*sp, Rc::new(self.ty(inner)?)),
            Ty::Ref(_, inner) => self.ty(inner)?,
            Ty::Array { metal: true, .. } => return Err(Error::unsupported("metal-array")),
            Ty::Array { elem, n, .. } => {
                let n: u32 = n
                    .parse()
                    .map_err(|_| Error::invalid(format!("array length {n}")))?;
                Type::Array(Rc::new(self.ty(elem)?), n)
            }
            Ty::Tex { kind, el, access } => {
                let k = TexKind::from_name(kind)
                    .ok_or_else(|| Error::unsupported(format!("texture:{kind}")))?;
                let a = Access::from_name(access)
                    .ok_or_else(|| Error::unsupported(format!("access:{access}")))?;
                Type::Texture(Tex {
                    kind: k,
                    elem: *el,
                    access: a,
                })
            }
            Ty::Sampler => Type::Sampler,
            Ty::Void => Type::Void,
            Ty::Opt(o) => return Err(Error::invalid(format!("option type {o}"))),
            Ty::Comp(c) => return Err(Error::invalid(format!("component type {c}"))),
        })
    }

    fn scalar(&self, n: &ScalarName) -> R<Scalar> {
        match n {
            ScalarName::Known(s) => Ok(*s),
            _ => Err(Error::invalid("bogus scalar")),
        }
    }

    // ------------------------------------------------------------------ items

    fn item(&mut self, item: &Item) -> R<()> {
        match item {
            Item::Preprocessor { text, .. } => {
                if text == msl_parse::parser::FP_CONTRACT_OFF {
                    self.contract_off = true;
                }
                Ok(())
            }
            Item::UsingNamespaceMetal { .. } => Ok(()),
            Item::Prelude { name, .. } => {
                if !matches!(
                    *name,
                    "dm_bs" | "dm_pool_mark" | "spvFAdd" | "spvDepthCast" | "spvUnsafeArray"
                ) {
                    return Err(Error::unsupported(format!("prelude:{name}")));
                }
                self.preludes.push(name);
                Ok(())
            }
            Item::Rejected { reason, .. } => Err(Error::unsupported(format!("rejected:{reason}"))),
            Item::Struct(s) => {
                let mut fields = Vec::with_capacity(s.fields.len());
                let mut metal = Vec::new();
                for (k, f) in s.fields.iter().enumerate() {
                    let ty = match &f.ty {
                        // `array<T, n>` of textures or samplers, only as an argument-buffer member
                        Ty::Array {
                            elem,
                            n,
                            metal: true,
                            ..
                        } => {
                            let n: u32 = n
                                .parse()
                                .map_err(|_| Error::invalid(format!("array length {n}")))?;
                            metal.push(k);
                            Type::Array(Rc::new(self.ty(elem)?), n)
                        }
                        t => self.ty(t)?,
                    };
                    fields.push((f.name.clone(), ty, f.attrs.clone(), f.is_const));
                }
                let id = self.out.types.add_struct(s.name.clone(), fields);
                for k in metal {
                    self.out.types.structs[id].fields[k].metal_array = true;
                }
                self.struct_ids.insert(s.name.clone(), id);
                Ok(())
            }
            Item::FunctionConstant {
                name, ty, attrs, ..
            } => {
                let t = self.ty(ty)?;
                let index = attrs
                    .first()
                    .and_then(|a| a.args.first())
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or_else(|| Error::invalid("function constant index"))?;
                if !matches!(t, Type::Scalar(_)) {
                    return Err(Error::unsupported("function-constant:non-scalar"));
                }
                self.fconst_ids.insert(name.clone(), self.out.fconsts.len());
                self.out.fconsts.push(FnConst {
                    name: name.clone(),
                    ty: t,
                    index,
                });
                Ok(())
            }
            Item::ConstArray { name, ty, init, .. } => {
                let t = self.ty(ty)?;
                let Type::Array(el, n) = &t else {
                    return Err(Error::invalid("const array type"));
                };
                if init.len() as u32 != *n {
                    return Err(Error::unsupported("const-array:partial-init"));
                }
                let mut vals = Vec::with_capacity(init.len());
                for e in init {
                    self.locals.clear();
                    let v = self.expr(e)?;
                    vals.push(conv(v, el)?);
                }
                self.const_ids.insert(name.clone(), self.out.consts.len());
                self.out.consts.push(ConstArray {
                    name: name.clone(),
                    ty: t,
                    init: vals,
                });
                Ok(())
            }
            Item::Function(f) => self.function(f),
        }
    }

    fn function(&mut self, f: &ast::Function) -> R<()> {
        self.locals.clear();
        self.ref_locals.clear();
        self.scopes = vec![HashMap::new()];
        let sig = &f.sig;
        self.ret = self.ty(&sig.ret)?;
        let mut params = Vec::with_capacity(sig.params.len());
        for p in &sig.params {
            let vt = self.ty(&p.ty)?;
            let by_ref = match &p.ty {
                Ty::Ref(sp, _) => Some(*sp),
                _ => None,
            };
            let lt = match by_ref {
                Some(sp) => Type::Ptr(sp, Rc::new(vt.clone())),
                None => vt.clone(),
            };
            let local = self.new_local(p.name.clone(), lt);
            if let Some(sp) = by_ref {
                self.ref_locals.insert(local, (sp, vt.clone()));
            }
            self.scopes[0].insert(p.name.clone(), local);
            params.push(Param {
                name: p.name.clone(),
                ty: vt,
                by_ref,
                local,
                attrs: p.attrs.clone(),
                is_const: p.is_const,
            });
        }
        if f.body.contract_off {
            return Err(Error::unsupported("fp-contract-body"));
        }
        let body = self.block(&f.body.stmts)?;
        let func = Func {
            name: sig.name.clone(),
            entry: sig.entry,
            ret: self.ret.clone(),
            params,
            locals: std::mem::take(&mut self.locals),
            body,
            attrs: f.attrs.clone(),
            inline: if f.always_inline {
                crate::hir::Inline::Always
            } else if f.is_inline {
                crate::hir::Inline::Hint
            } else {
                crate::hir::Inline::None
            },
            template: false,
            contract: !self.contract_off,
        };
        self.sigs.push((sig.clone(), self.out.funcs.len()));
        self.out.funcs.push(func);
        Ok(())
    }

    fn new_local(&mut self, name: Rc<str>, ty: Type) -> LocalId {
        self.locals.push(Local { name, ty });
        self.locals.len() - 1
    }

    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    // ------------------------------------------------------------------ statements

    fn block(&mut self, stmts: &[ast::Stmt]) -> R<Vec<Stmt>> {
        self.scopes.push(HashMap::new());
        let mut out = Vec::with_capacity(stmts.len());
        for s in stmts {
            self.stmt(s, &mut out)?;
        }
        self.scopes.pop();
        Ok(out)
    }

    /// A sub-statement in its own scope.
    fn sub(&mut self, s: &ast::Stmt) -> R<Vec<Stmt>> {
        self.block(std::slice::from_ref(s))
    }

    fn cond(&mut self, e: &ast::Expr) -> R<Expr> {
        let v = self.expr(e)?;
        conv(v, &Type::BOOL)
    }

    fn stmt(&mut self, s: &ast::Stmt, out: &mut Vec<Stmt>) -> R<()> {
        match s {
            ast::Stmt::Empty => {}
            ast::Stmt::Block(b) => {
                let stmts = self.block(&b.stmts)?;
                out.push(if b.contract_off {
                    Stmt::NoContract(stmts)
                } else {
                    Stmt::Block(stmts)
                });
            }
            ast::Stmt::If { cond, then, els } => {
                let c = self.cond(cond)?;
                let t = self.sub(then)?;
                let e = match els {
                    Some(e) => self.sub(e)?,
                    None => Vec::new(),
                };
                out.push(Stmt::If {
                    cond: c,
                    then: t,
                    els: e,
                });
            }
            ast::Stmt::Switch { cond, body } => {
                let c = self.expr(cond)?;
                if !matches!(&c.ty, Type::Scalar(s) if !is_float(*s) && *s != Scalar::Bool) {
                    return Err(Error::invalid("switch on non-integer"));
                }
                self.scopes.push(HashMap::new());
                let mut arms: Vec<SwitchArm> = Vec::new();
                for entry in body {
                    match entry {
                        SwitchEntry::Case(text) => {
                            let v = parse_int(text)?;
                            push_label(&mut arms, Some(v as i64));
                        }
                        SwitchEntry::Default => push_label(&mut arms, None),
                        SwitchEntry::Stmt(st) => {
                            let Some(arm) = arms.last_mut() else {
                                return Err(Error::invalid("statement before first case"));
                            };
                            let mut b = std::mem::take(&mut arm.body);
                            self.stmt(st, &mut b)?;
                            arms.last_mut().expect("arm").body = b;
                        }
                    }
                }
                self.scopes.pop();
                out.push(Stmt::Switch { cond: c, arms });
            }
            ast::Stmt::DoWhile { body, cond } => {
                let b = self.sub(body)?;
                let c = self.cond(cond)?;
                out.push(Stmt::DoWhile { body: b, cond: c });
            }
            ast::Stmt::While { cond, body } => {
                let c = self.cond(cond)?;
                let b = self.sub(body)?;
                out.push(Stmt::While { cond: c, body: b });
            }
            ast::Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.scopes.push(HashMap::new());
                let mut i = Vec::new();
                match init {
                    Some(ast::ForInit::Decl(d)) => self.decl(d, &mut i)?,
                    Some(ast::ForInit::Expr(e)) => i.push(Stmt::Expr(self.expr(e)?)),
                    None => {}
                }
                let c = match cond {
                    Some(c) => Some(self.cond(c)?),
                    None => None,
                };
                let st = match step {
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                let b = self.sub(body)?;
                self.scopes.pop();
                out.push(Stmt::For {
                    init: i,
                    cond: c,
                    step: st,
                    body: b,
                });
            }
            ast::Stmt::Break => out.push(Stmt::Break),
            ast::Stmt::Continue => out.push(Stmt::Continue),
            ast::Stmt::Return(v) => {
                let v = match v {
                    Some(e) => {
                        let x = self.expr(e)?;
                        let rt = self.ret.clone();
                        Some(conv(x, &rt)?)
                    }
                    None => None,
                };
                out.push(Stmt::Return(v));
            }
            ast::Stmt::DiscardFragment => out.push(Stmt::Discard),
            ast::Stmt::Decl(d) => self.decl(d, out)?,
            ast::Stmt::Expr(e) => out.push(Stmt::Expr(self.expr(e)?)),
        }
        Ok(())
    }

    fn decl(&mut self, d: &ast::Decl, out: &mut Vec<Stmt>) -> R<()> {
        for v in &d.vars {
            let t = self.ty(&v.ty)?;
            // The initialiser sees the scope before the name is declared.
            let init = match &v.init {
                Init::None => None,
                Init::Zero => Some(Expr::new(ExprKind::Zero, t.unpacked())),
                Init::Expr(e) => {
                    let x = self.expr(e)?;
                    Some(conv(x, &t)?)
                }
            };
            let id = self.new_local(v.name.clone(), t.clone());
            out.push(Stmt::Decl(id));
            self.scopes
                .last_mut()
                .expect("scope")
                .insert(v.name.clone(), id);
            if let Some(x) = init {
                let place = Place {
                    kind: PlaceKind::Local(id),
                    ty: t.clone(),
                    space: Space::Thread,
                };
                out.push(Stmt::Expr(Expr::new(
                    ExprKind::Assign(Box::new(place), Box::new(x)),
                    t.unpacked(),
                )));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ places

    /// True when the expression designates storage (no temporary needed).
    fn is_place(&self, e: &ast::Expr) -> bool {
        match &e.kind {
            A::Var { name, scope } => match scope {
                VarScope::Local => true,
                VarScope::Global => self.const_ids.contains_key(name),
            },
            A::Deref(_) => true,
            A::Paren(x) => self.is_place(x),
            A::Member { base, arrow, .. } => *arrow || self.is_place(base),
            A::Index { base, .. } => matches!(base.ty.strip(), Ty::Ptr(..)) || self.is_place(base),
            A::Swizzle { base, .. } => self.is_place(base),
            _ => false,
        }
    }

    /// A place for an expression, with a temporary for an rvalue.
    fn place(&mut self, e: &ast::Expr) -> R<Place> {
        if !self.is_place(e) {
            let v = self.expr(e)?;
            let ty = v.ty.clone();
            return Ok(Place {
                kind: PlaceKind::Temp(Box::new(v)),
                ty,
                space: Space::Thread,
            });
        }
        match &e.kind {
            A::Paren(x) => self.place(x),
            A::Var { name, scope } => match scope {
                VarScope::Local => {
                    let id = self
                        .lookup(name)
                        .ok_or_else(|| Error::invalid(format!("unknown local {name}")))?;
                    if let Some((sp, vt)) = self.ref_locals.get(&id).cloned() {
                        let pt = self.locals[id].ty.clone();
                        let ptr = load_local(id, pt);
                        return Ok(Place {
                            kind: PlaceKind::Deref(Box::new(ptr)),
                            ty: vt,
                            space: sp,
                        });
                    }
                    let ty = self.locals[id].ty.clone();
                    Ok(Place {
                        kind: PlaceKind::Local(id),
                        ty,
                        space: Space::Thread,
                    })
                }
                VarScope::Global => {
                    let id = self.const_ids[name];
                    let ty = self.out.consts[id].ty.clone();
                    Ok(Place {
                        kind: PlaceKind::Const(id),
                        ty,
                        space: Space::Constant,
                    })
                }
            },
            A::Deref(p) => {
                let ptr = self.expr(p)?;
                deref(ptr)
            }
            A::Member { base, field, arrow } => {
                let bp = if *arrow {
                    deref(self.expr(base)?)?
                } else {
                    self.place(base)?
                };
                self.field(bp, field)
            }
            A::Index { base, index } => {
                let idx = self.index(index)?;
                if let Ty::Ptr(..) = base.ty.strip() {
                    let ptr = self.expr(base)?;
                    let pty = ptr.ty.clone();
                    return deref(Expr::new(
                        ExprKind::PtrOffset(Box::new(ptr), Box::new(idx)),
                        pty,
                    ));
                }
                let bp = self.place(base)?;
                match bp.ty.clone() {
                    Type::Array(el, _) => {
                        let sp = bp.space;
                        Ok(Place {
                            kind: PlaceKind::Index(Box::new(bp), Box::new(idx)),
                            ty: (*el).clone(),
                            space: sp,
                        })
                    }
                    Type::Vector(s, _) | Type::Packed(s, _) => {
                        let sp = bp.space;
                        Ok(Place {
                            kind: PlaceKind::Element(Box::new(bp), Box::new(idx)),
                            ty: Type::Scalar(s),
                            space: sp,
                        })
                    }
                    t => Err(Error::unsupported(format!("index-on:{t}"))),
                }
            }
            A::Swizzle { base, comps } => {
                let bp = self.place(base)?;
                let Some(s) = bp.ty.elem() else {
                    return Err(Error::invalid("swizzle of non-vector"));
                };
                let sp = bp.space;
                if comps.len() == 1 {
                    // a one-component swizzle stays a swizzle: clang loads the vector untagged for it
                    return Ok(Place {
                        kind: PlaceKind::Swizzle(Box::new(bp), comps.clone()),
                        ty: Type::Scalar(s),
                        space: sp,
                    });
                }
                let ty = Type::Vector(s, comps.len() as u8);
                Ok(Place {
                    kind: PlaceKind::Swizzle(Box::new(bp), comps.clone()),
                    ty,
                    space: sp,
                })
            }
            _ => Err(Error::invalid("place")),
        }
    }

    fn field(&self, bp: Place, field: &str) -> R<Place> {
        let Type::Struct(id) = bp.ty else {
            return Err(Error::invalid("member of non-struct"));
        };
        let def = &self.out.types.structs[id];
        let (i, f) = def
            .fields
            .iter()
            .enumerate()
            .find(|(_, f)| &*f.name == field)
            .ok_or_else(|| Error::invalid(format!("no field {field}")))?;
        let ty = f.ty.clone();
        let sp = bp.space;
        Ok(Place {
            kind: PlaceKind::Field(Box::new(bp), i as u32),
            ty,
            space: sp,
        })
    }

    fn index(&mut self, e: &ast::Expr) -> R<Expr> {
        let v = self.expr(e)?;
        match v.ty {
            Type::Scalar(s) if !is_float(s) && s != Scalar::Bool => Ok(v),
            _ => Err(Error::invalid("non-integer index")),
        }
    }

    // ------------------------------------------------------------------ expressions

    fn expr(&mut self, e: &ast::Expr) -> R<Expr> {
        let ty = self.ty(&e.ty.strip())?.unpacked();
        match &e.kind {
            A::Literal(text) => match ty {
                Type::Scalar(s) if is_float(s) => {
                    Ok(Expr::new(ExprKind::Float(parse_float(text)?), ty))
                }
                Type::Scalar(_) => Ok(Expr::int(parse_int(text)?, ty)),
                _ => Err(Error::invalid("literal type")),
            },
            A::Bool(b) => Ok(Expr::int(*b as i128, Type::BOOL)),
            A::Var {
                name,
                scope: VarScope::Global,
            } if self.fconst_ids.contains_key(name) => {
                let id = self.fconst_ids[name];
                let t = self.out.fconsts[id].ty.clone();
                Ok(Expr::new(ExprKind::FnConst(id), t))
            }
            A::Var { .. } | A::Deref(_) | A::Member { .. } | A::Index { .. } => {
                if !self.is_place(e) {
                    // member or index of an rvalue
                    let p = self.place_of_rvalue(e)?;
                    return Ok(load(p));
                }
                let p = self.place(e)?;
                Ok(load(p))
            }
            A::Swizzle { base, comps } => {
                if self.is_place(e) {
                    let p = self.place(e)?;
                    return Ok(load(p));
                }
                let b = self.expr(base)?;
                Ok(Expr::new(ExprKind::Swizzle(Box::new(b), comps.clone()), ty))
            }
            A::Paren(x) => self.expr(x),
            A::AddrOf(x) => {
                let p = self.place(x)?;
                Ok(Expr::new(ExprKind::AddrOf(Box::new(p)), ty))
            }
            A::Unary { op, operand } => {
                let v = self.expr(operand)?;
                match op {
                    ast::UnOp::Plus => conv(v, &ty),
                    ast::UnOp::Neg => Ok(Expr::new(
                        ExprKind::Unary(UnOp::Neg, Box::new(conv(v, &ty)?)),
                        ty,
                    )),
                    ast::UnOp::BitNot => Ok(Expr::new(
                        ExprKind::Unary(UnOp::BitNot, Box::new(conv(v, &ty)?)),
                        ty,
                    )),
                    ast::UnOp::Not => Ok(Expr::new(
                        ExprKind::Unary(UnOp::Not, Box::new(conv(v, &Type::BOOL)?)),
                        ty,
                    )),
                }
            }
            A::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, ty),
            A::Assign { op, lhs, rhs } => {
                let p = self.place(lhs)?;
                let v = self.expr(rhs)?;
                match op {
                    None => {
                        let pt = p.ty.unpacked();
                        let v = conv(v, &pt)?;
                        Ok(Expr::new(ExprKind::Assign(Box::new(p), Box::new(v)), pt))
                    }
                    Some(bop) => {
                        let pt = p.ty.unpacked();
                        let op_ty = if matches!(bop, ast::BinOp::Shl | ast::BinOp::Shr) {
                            promote(&pt)?
                        } else {
                            common(&pt, &v.ty)?
                        };
                        let v = conv(v, &op_ty)?;
                        let hop = arith_op(*bop).ok_or_else(|| Error::invalid("compound op"))?;
                        Ok(Expr::new(
                            ExprKind::CompoundAssign {
                                op: hop,
                                place: Box::new(p),
                                value: Box::new(v),
                                op_ty,
                            },
                            pt,
                        ))
                    }
                }
            }
            A::Cond { cond, then, els } => {
                let c = self.cond(cond)?;
                let a = conv(self.expr(then)?, &ty)?;
                let b = conv(self.expr(els)?, &ty)?;
                Ok(Expr::new(
                    ExprKind::Cond(Box::new(c), Box::new(a), Box::new(b)),
                    ty,
                ))
            }
            A::Call { callee, args } => self.call(callee, args, ty),
            A::Method { base, name, args } => self.method(base, name, args, ty),
            A::Construct { args } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.expr(a)?);
                }
                match &ty {
                    Type::Scalar(_) => {
                        let v = vals.pop().ok_or_else(|| Error::invalid("scalar ctor"))?;
                        conv(v, &ty)
                    }
                    Type::Vector(s, n) => {
                        if vals.is_empty() {
                            return Ok(Expr::new(ExprKind::Zero, ty));
                        }
                        if vals.len() == 1 {
                            let v = vals.pop().expect("one");
                            if v.ty.width() == Some(1) {
                                let el = conv(v, &Type::Scalar(*s))?;
                                return Ok(Expr::new(ExprKind::Splat(Box::new(el)), ty));
                            }
                            if v.ty.width() == Some(*n) {
                                return conv(v, &ty);
                            }
                            return Err(Error::invalid("vector ctor width"));
                        }
                        let mut parts = Vec::with_capacity(vals.len());
                        for v in vals {
                            let w = v.ty.width().ok_or_else(|| Error::invalid("ctor part"))?;
                            parts.push(conv(v, &Type::vec(*s, w))?);
                        }
                        Ok(Expr::new(ExprKind::Vector(parts), ty))
                    }
                    _ => Err(Error::unsupported(format!("ctor:{ty}"))),
                }
            }
            A::Cast { kind, operand, .. } => {
                let v = self.expr(operand)?;
                match kind {
                    CastKind::AsType => {
                        if v.ty == ty {
                            return Ok(v);
                        }
                        Ok(Expr::new(ExprKind::Bitcast(Box::new(v)), ty))
                    }
                    CastKind::CStyle | CastKind::Static => match (&v.ty, &ty) {
                        (Type::Ptr(..), Type::Ptr(..)) => {
                            Ok(Expr::new(ExprKind::PtrCast(Box::new(v)), ty))
                        }
                        _ => conv(v, &ty),
                    },
                    CastKind::Reinterpret => match (&v.ty, &ty) {
                        (
                            Type::Ptr(..) | Type::Scalar(Scalar::Ulong | Scalar::Long),
                            Type::Ptr(..),
                        ) => Ok(Expr::new(ExprKind::PtrCast(Box::new(v)), ty)),
                        _ => Err(Error::unsupported("reinterpret_cast")),
                    },
                }
            }
            A::StructInit { args, .. } => {
                let Type::Struct(id) = ty else {
                    return Err(Error::invalid("struct init type"));
                };
                let ftys: Vec<Type> = self.out.types.structs[id]
                    .fields
                    .iter()
                    .map(|f| f.ty.clone())
                    .collect();
                let mut vals = Vec::with_capacity(args.len());
                for (a, ft) in args.iter().zip(ftys.iter()) {
                    let v = self.expr(a)?;
                    vals.push(conv(v, ft)?);
                }
                Ok(Expr::new(ExprKind::StructInit(vals), ty))
            }
            A::Component(_) => Err(Error::invalid("component outside gather")),
        }
    }

    /// Member or index of an rvalue: the value goes to a temporary first.
    fn place_of_rvalue(&mut self, e: &ast::Expr) -> R<Place> {
        match &e.kind {
            A::Member {
                base,
                field,
                arrow: false,
            } => {
                let bp = self.place(base)?;
                self.field(bp, field)
            }
            A::Index { base, index } => {
                let idx = self.index(index)?;
                let bp = self.place(base)?;
                match bp.ty.clone() {
                    Type::Array(el, _) => {
                        let sp = bp.space;
                        Ok(Place {
                            kind: PlaceKind::Index(Box::new(bp), Box::new(idx)),
                            ty: (*el).clone(),
                            space: sp,
                        })
                    }
                    Type::Vector(s, _) => {
                        let sp = bp.space;
                        Ok(Place {
                            kind: PlaceKind::Element(Box::new(bp), Box::new(idx)),
                            ty: Type::Scalar(s),
                            space: sp,
                        })
                    }
                    t => Err(Error::unsupported(format!("index-on:{t}"))),
                }
            }
            _ => self.place(e),
        }
    }

    fn binary(&mut self, op: ast::BinOp, lhs: &ast::Expr, rhs: &ast::Expr, ty: Type) -> R<Expr> {
        use ast::BinOp as B;
        let a = self.expr(lhs)?;
        let b = self.expr(rhs)?;
        match op {
            B::LogAnd | B::LogOr => {
                let a = conv(a, &Type::BOOL)?;
                let b = conv(b, &Type::BOOL)?;
                Ok(Expr::new(
                    ExprKind::Logical(op == B::LogAnd, Box::new(a), Box::new(b)),
                    Type::BOOL,
                ))
            }
            B::Eq | B::Ne | B::Lt | B::Gt | B::Le | B::Ge => {
                let ct = common(&a.ty, &b.ty)?;
                let a = conv(a, &ct)?;
                let b = conv(b, &ct)?;
                let c = match op {
                    B::Eq => CmpOp::Eq,
                    B::Ne => CmpOp::Ne,
                    B::Lt => CmpOp::Lt,
                    B::Gt => CmpOp::Gt,
                    B::Le => CmpOp::Le,
                    _ => CmpOp::Ge,
                };
                Ok(Expr::new(
                    ExprKind::Compare(c, Box::new(a), Box::new(b)),
                    ty,
                ))
            }
            _ => {
                if let Type::Ptr(..) = a.ty {
                    let mut off = b;
                    if op == B::Sub {
                        let t = off.ty.clone();
                        off = Expr::new(ExprKind::Unary(UnOp::Neg, Box::new(off)), t);
                    }
                    return Ok(Expr::new(
                        ExprKind::PtrOffset(Box::new(a), Box::new(off)),
                        ty,
                    ));
                }
                let hop = arith_op(op).ok_or_else(|| Error::invalid("binary op"))?;
                let a = conv(a, &ty)?;
                let b = conv(b, &ty)?;
                Ok(Expr::new(
                    ExprKind::Binary(hop, Box::new(a), Box::new(b)),
                    ty,
                ))
            }
        }
    }

    fn call(&mut self, callee: &Callee, args: &[ast::Expr], ty: Type) -> R<Expr> {
        match callee {
            Callee::User(sig) => {
                let fid = self
                    .sigs
                    .iter()
                    .find(|(s, _)| Rc::ptr_eq(s, sig))
                    .map(|(_, id)| *id)
                    .ok_or_else(|| Error::invalid(format!("call of undefined {}", sig.name)))?;
                let mut out = Vec::with_capacity(args.len());
                let params: Vec<(Option<Space>, Type)> = self.out.funcs[fid]
                    .params
                    .iter()
                    .map(|p| (p.by_ref, p.ty.clone()))
                    .collect();
                for (a, (by_ref, pt)) in args.iter().zip(params.iter()) {
                    if by_ref.is_some() {
                        let p = self.place(a)?;
                        out.push(Arg::Ref(p));
                    } else {
                        let v = self.expr(a)?;
                        out.push(Arg::Value(conv(v, pt)?));
                    }
                }
                Ok(Expr::new(ExprKind::Call(fid, out), ty))
            }
            Callee::Prelude("spvFAdd") => {
                let mut vals = Vec::new();
                for a in args {
                    let v = self.expr(a)?;
                    vals.push(conv(v, &ty)?);
                }
                let one = match &ty {
                    Type::Scalar(s) => Expr::new(ExprKind::Float(1.0), Type::Scalar(*s)),
                    Type::Vector(s, _) => Expr::new(
                        ExprKind::Splat(Box::new(Expr::new(
                            ExprKind::Float(1.0),
                            Type::Scalar(*s),
                        ))),
                        ty.clone(),
                    ),
                    _ => return Err(Error::unsupported("spvFAdd:type")),
                };
                vals.insert(0, one);
                Ok(Expr::new(ExprKind::Builtin("fma", vals), ty))
            }
            Callee::Prelude("spvDepthCast") => {
                let v = self.expr(&args[0])?;
                Ok(Expr::new(ExprKind::TexCast(Box::new(v)), ty))
            }
            Callee::Prelude("dm_bs") => self.bs_call(args, ty),
            Callee::Prelude("dm_pool_mark") => self.pool_mark(args),
            Callee::Prelude(p) => Err(Error::unsupported(format!("prelude-call:{p}"))),
            Callee::Builtin(name) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.expr(a)?);
                }
                let (n, vals) = builtins::bind(name, vals, &ty)?;
                Ok(Expr::new(ExprKind::Builtin(n, vals), ty))
            }
        }
    }

    /// `dm_bs(r, b, i, t, w, a...)`: `if (b->mask >> i & 1u) r = dm_bmix(r, t.sample(w, a...), b->k[i]); return r;`
    fn bs_call(&mut self, args: &[ast::Expr], ty: Type) -> R<Expr> {
        if args.len() < 6 {
            return Err(Error::invalid("dm_bs arity"));
        }
        // dm_bs is a template: clang instantiates it as an always-inline function per argument types.
        // its arguments are evaluated at the call, its body sits in the instantiation
        let mut vals = vec![
            conv(self.expr(&args[0])?, &ty)?,
            self.expr(&args[1])?,
            conv(self.expr(&args[2])?, &Type::UINT)?,
            self.expr(&args[3])?,
            self.expr(&args[4])?,
        ];
        // a sampling option (`level(x)`) is passed as its struct, whose value is the float
        let mut opts: Vec<Option<&'static str>> = Vec::new();
        for a in &args[5..] {
            if let A::Call {
                callee: Callee::Builtin(n),
                args: oa,
            } = &a.kind
            {
                if matches!(&**n, "level" | "bias") {
                    vals.push(conv(self.expr(&oa[0])?, &Type::FLOAT)?);
                    opts.push(Some(if &**n == "level" { "level" } else { "bias" }));
                    continue;
                }
            }
            vals.push(self.expr(a)?);
            opts.push(None);
        }
        let Type::Texture(tx) = vals[3].ty else {
            return Err(Error::invalid("dm_bs texture"));
        };
        let tys: Vec<Type> = vals.iter().map(|v| v.ty.clone()).collect();
        let fid = match self
            .bs_inst
            .iter()
            .find(|(t, o, _)| *t == tys && *o == opts)
        {
            Some((_, _, f)) => *f,
            None => {
                let f = self.bs_instantiate(&tys, &opts, tx, &ty)?;
                self.bs_inst.push((tys, opts, f));
                f
            }
        };
        let args = vals.into_iter().map(Arg::Value).collect();
        Ok(Expr::new(ExprKind::Call(fid, args), ty))
    }

    /// Builds `V dm_bs(V r, constant dm_border_t* b, uint i, T t, sampler w, A... a)`:
    /// `if (b->mask >> i & 1u) r = dm_bmix(r, t.sample(w, a...), b->k[i]); return r;`
    fn bs_instantiate(
        &mut self,
        tys: &[Type],
        opts: &[Option<&'static str>],
        tx: Tex,
        ty: &Type,
    ) -> R<usize> {
        let saved = (
            std::mem::take(&mut self.locals),
            std::mem::take(&mut self.ref_locals),
            std::mem::take(&mut self.scopes),
            std::mem::replace(&mut self.ret, ty.clone()),
        );
        let names = ["r", "b", "i", "t", "w"];
        let mut params = Vec::new();
        for (k, t) in tys.iter().enumerate() {
            let name: Rc<str> = if k < 5 {
                names[k].into()
            } else {
                format!("a{}", k - 5).into()
            };
            let local = self.new_local(name.clone(), t.clone());
            params.push(Param {
                name,
                ty: t.clone(),
                by_ref: None,
                local,
                attrs: Vec::new(),
                is_const: false,
            });
        }
        let (rl, bl, il, tl, wl) = (0, 1, 2, 3, 4);
        let border = deref(load_local(bl, tys[1].clone()))?;
        let mask = load(self.field(border.clone(), "mask")?);
        let shifted = Expr::new(
            ExprKind::Binary(
                BinOp::Shr,
                Box::new(mask),
                Box::new(load_local(il, Type::UINT)),
            ),
            Type::UINT,
        );
        let bit = Expr::new(
            ExprKind::Binary(
                BinOp::And,
                Box::new(shifted),
                Box::new(Expr::int(1, Type::UINT)),
            ),
            Type::UINT,
        );
        let cond = conv(bit, &Type::BOOL)?;
        let extra: Vec<SampleArg> = (5..tys.len())
            .map(|k| match opts[k - 5] {
                Some(o) => SampleArg::Opt(o, k),
                None => SampleArg::Local(k),
            })
            .collect();
        let sample = self.sample_from(
            tx,
            load_local(tl, Type::Texture(tx)),
            load_local(wl, Type::Sampler),
            &extra,
        )?;
        let k = self.field(border, "k")?;
        let Type::Array(kel, _) = k.ty.clone() else {
            return Err(Error::invalid("dm_border_t.k"));
        };
        let ki = Place {
            kind: PlaceKind::Index(Box::new(k.clone()), Box::new(load_local(il, Type::UINT))),
            ty: (*kel).clone(),
            space: k.space,
        };
        let mix = self.user_call(
            "dm_bmix",
            vec![load_local(rl, ty.clone()), sample, load(ki)],
            ty,
        )?;
        let body = vec![
            Stmt::If {
                cond,
                then: vec![assign_local(rl, mix)],
                els: Vec::new(),
            },
            Stmt::Return(Some(load_local(rl, ty.clone()))),
        ];
        let locals = std::mem::take(&mut self.locals);
        (self.locals, self.ref_locals, self.scopes, self.ret) = saved;
        self.out.funcs.push(Func {
            name: "dm_bs".into(),
            entry: None,
            ret: ty.clone(),
            params,
            locals,
            body,
            attrs: Vec::new(),
            inline: crate::hir::Inline::Always,
            template: true,
            contract: !self.contract_off,
        });
        Ok(self.out.funcs.len() - 1)
    }

    /// Calls a user function by name, choosing the overload whose parameter types equal the argument types.
    fn user_call(&self, name: &str, args: Vec<Expr>, ret: &Type) -> R<Expr> {
        for (fid, f) in self.out.funcs.iter().enumerate() {
            if &*f.name != name || f.params.len() != args.len() || f.ret != *ret {
                continue;
            }
            if f.params
                .iter()
                .zip(args.iter())
                .all(|(p, a)| p.by_ref.is_none() && p.ty.unpacked() == a.ty)
            {
                let args = args.into_iter().map(Arg::Value).collect();
                return Ok(Expr::new(ExprKind::Call(fid, args), ret.clone()));
            }
        }
        Err(Error::unsupported(format!("helper-overload:{name}")))
    }

    fn method(&mut self, base: &ast::Expr, name: &str, args: &[ast::Expr], ty: Type) -> R<Expr> {
        let tex = self.expr(base)?;
        let Type::Texture(tx) = tex.ty else {
            return Err(Error::invalid("method on non-texture"));
        };
        let tex_b = Box::new(tex.clone());
        let lod_arg = |cx: &mut Cx, a: Option<&ast::Expr>| -> R<Option<Box<Expr>>> {
            match a {
                Some(a) => Ok(Some(Box::new(conv(cx.expr(a)?, &Type::UINT)?))),
                None => Ok(None),
            }
        };
        let op = match name {
            "sample" | "sample_compare" | "gather" => {
                let extra: Vec<SampleArg> = args.iter().map(SampleArg::Ast).collect();
                let e = self.sample_op(tx, tex, name, &extra)?;
                return Ok(Expr { ty, ..e });
            }
            "read" => {
                let dim = tx.kind.coord_dim();
                let coord = conv(self.expr(&args[0])?, &Type::vec(Scalar::Uint, dim))?;
                let mut rest = &args[1..];
                let array = if tx.kind.is_array() {
                    let a = rest.first().ok_or_else(|| Error::invalid("read slice"))?;
                    rest = &rest[1..];
                    Some(Box::new(conv(self.expr(a)?, &Type::UINT)?))
                } else {
                    None
                };
                if tx.kind == TexKind::T2dMs {
                    return Err(Error::unsupported("method:read:texture2d_ms"));
                }
                let lod = lod_arg(self, rest.first())?;
                TexOp::Read {
                    coord: Box::new(coord),
                    array,
                    lod,
                }
            }
            "write" => {
                let value = conv(self.expr(&args[0])?, &Type::Vector(tx.elem, 4))?;
                let dim = tx.kind.coord_dim();
                let coord = conv(self.expr(&args[1])?, &Type::vec(Scalar::Uint, dim))?;
                let mut rest = &args[2..];
                let array = if tx.kind.is_array() {
                    let a = rest.first().ok_or_else(|| Error::invalid("write slice"))?;
                    rest = &rest[1..];
                    Some(Box::new(conv(self.expr(a)?, &Type::UINT)?))
                } else {
                    None
                };
                let lod = lod_arg(self, rest.first())?;
                TexOp::Write {
                    value: Box::new(value),
                    coord: Box::new(coord),
                    array,
                    lod,
                }
            }
            "get_width" | "get_height" | "get_depth" | "get_array_size" | "get_num_mip_levels" => {
                let what = match name {
                    "get_width" => Query::Width,
                    "get_height" => Query::Height,
                    "get_depth" => Query::Depth,
                    "get_array_size" => Query::ArraySize,
                    _ => Query::MipLevels,
                };
                let lod = if matches!(what, Query::Width | Query::Height | Query::Depth) {
                    lod_arg(self, args.first())?
                } else {
                    None
                };
                TexOp::Query { what, lod }
            }
            "calculate_clamped_lod" | "calculate_unclamped_lod" => {
                let sampler = self.expr(&args[0])?;
                let coord = conv(
                    self.expr(&args[1])?,
                    &Type::vec(Scalar::Float, tx.kind.coord_dim()),
                )?;
                TexOp::Lod {
                    clamped: name == "calculate_clamped_lod",
                    sampler: Box::new(sampler),
                    coord: Box::new(coord),
                }
            }
            _ => return Err(Error::unsupported(format!("method:{name}"))),
        };
        Ok(Expr::new(
            ExprKind::Texture {
                tex: tex_b,
                tex_ty: tx,
                op,
            },
            ty,
        ))
    }

    fn sample_from(
        &mut self,
        tx: Tex,
        tex: Expr,
        sampler: Expr,
        rest: &[SampleArg<'_>],
    ) -> R<Expr> {
        let mut all = vec![SampleArg::Value(sampler)];
        all.extend(rest.iter().cloned());
        self.sample_op(tx, tex, "sample", &all)
    }

    /// `sample`, `sample_compare`, `gather` with arguments (sampler, coord, [slice], [ref], options...).
    fn sample_op(&mut self, tx: Tex, tex: Expr, name: &str, args: &[SampleArg<'_>]) -> R<Expr> {
        let mut it = args.iter();
        let sampler = self.sample_arg(it.next().ok_or_else(|| Error::invalid("sampler"))?)?;
        let dim = tx.kind.coord_dim();
        let coord = self.sample_arg(it.next().ok_or_else(|| Error::invalid("coord"))?)?;
        let coord = conv(coord, &Type::vec(Scalar::Float, dim))?;
        let array = if tx.kind.is_array() {
            let a = self.sample_arg(it.next().ok_or_else(|| Error::invalid("slice"))?)?;
            Some(Box::new(conv(a, &Type::UINT)?))
        } else {
            None
        };
        let compare = if name == "sample_compare" {
            let r = self.sample_arg(it.next().ok_or_else(|| Error::invalid("compare ref"))?)?;
            Some(Box::new(conv(r, &Type::FLOAT)?))
        } else {
            None
        };
        let mut opt = SampleOpt::None;
        let mut offset = None;
        let mut gather = if name == "gather" { Some(0u8) } else { None };
        for a in it {
            if let SampleArg::Opt(o, l) = a {
                let v = load_local(*l, Type::FLOAT);
                opt = if *o == "level" {
                    SampleOpt::Level(Box::new(v))
                } else {
                    SampleOpt::Bias(Box::new(v))
                };
                continue;
            }
            if let SampleArg::Ast(e) = a {
                match &e.kind {
                    A::Call {
                        callee: Callee::Builtin(n),
                        args: oargs,
                    } if matches!(&**n, "level" | "bias") => {
                        let v = conv(self.expr(&oargs[0])?, &Type::FLOAT)?;
                        opt = if &**n == "level" {
                            SampleOpt::Level(Box::new(v))
                        } else {
                            SampleOpt::Bias(Box::new(v))
                        };
                        continue;
                    }
                    A::Call {
                        callee: Callee::Builtin(n),
                        ..
                    } => {
                        return Err(Error::unsupported(format!("sample-option:{n}")));
                    }
                    A::Component(c) => {
                        let i = match &**c {
                            "x" => 0,
                            "y" => 1,
                            "z" => 2,
                            "w" => 3,
                            _ => return Err(Error::invalid("component")),
                        };
                        gather = Some(i);
                        continue;
                    }
                    _ => {}
                }
            }
            let v = self.sample_arg(a)?;
            if dim == 3 && tx.kind.is_cube() {
                return Err(Error::unsupported("sample-offset:cube"));
            }
            offset = Some(Box::new(conv(v, &Type::vec(Scalar::Int, dim))?));
        }
        if name == "gather" && !matches!(opt, SampleOpt::None) {
            return Err(Error::invalid("gather option"));
        }
        let ty = if name == "sample_compare" || (tx.kind.is_depth() && name == "sample") {
            Type::FLOAT
        } else if tx.kind.is_depth() {
            Type::Vector(Scalar::Float, 4)
        } else {
            Type::Vector(tx.elem, 4)
        };
        let op = TexOp::Sample {
            gather,
            sampler: Box::new(sampler),
            coord: Box::new(coord),
            array,
            compare,
            opt,
            offset,
        };
        Ok(Expr::new(
            ExprKind::Texture {
                tex: Box::new(tex),
                tex_ty: tx,
                op,
            },
            ty,
        ))
    }

    fn sample_arg(&mut self, a: &SampleArg<'_>) -> R<Expr> {
        match a {
            SampleArg::Ast(e) => self.expr(e),
            SampleArg::Local(l) => Ok(load_local(*l, self.locals[*l].ty.clone())),
            SampleArg::Value(v) => Ok(v.clone()),
            SampleArg::Opt(..) => Err(Error::invalid("option as value")),
        }
    }
}

/// An argument of a sampling call: from the tree, a temporary, or a value.
#[derive(Clone)]
enum SampleArg<'a> {
    Ast(&'a ast::Expr),
    Local(LocalId),
    Value(Expr),
    /// A sampling option whose float value is in a local.
    Opt(&'static str, LocalId),
}

fn push_label(arms: &mut Vec<SwitchArm>, v: Option<i64>) {
    match arms.last_mut() {
        Some(a) if a.body.is_empty() => a.labels.push(v),
        _ => arms.push(SwitchArm {
            labels: vec![v],
            body: Vec::new(),
        }),
    }
}

fn arith_op(op: ast::BinOp) -> Option<BinOp> {
    use ast::BinOp as B;
    Some(match op {
        B::Add => BinOp::Add,
        B::Sub => BinOp::Sub,
        B::Mul => BinOp::Mul,
        B::Div => BinOp::Div,
        B::Rem => BinOp::Rem,
        B::BitAnd => BinOp::And,
        B::BitOr => BinOp::Or,
        B::BitXor => BinOp::Xor,
        B::Shl => BinOp::Shl,
        B::Shr => BinOp::Shr,
        _ => return None,
    })
}

/// The integer promotion of a scalar type (types narrower than int become int).
fn promote(t: &Type) -> R<Type> {
    Ok(match t {
        Type::Scalar(s) if !is_float(*s) && s.rank().unwrap_or(9) < 3 => Type::INT,
        t => t.clone(),
    })
}

/// The common type of two numeric operands (usual arithmetic conversions, vectors win).
fn common(a: &Type, b: &Type) -> R<Type> {
    match (a, b) {
        (Type::Vector(s, n), _) => Ok(Type::Vector(*s, *n)),
        (_, Type::Vector(s, n)) => Ok(Type::Vector(*s, *n)),
        (Type::Scalar(x), Type::Scalar(y)) => {
            let r = arith(&ScalarName::Known(*x), &ScalarName::Known(*y))
                .map_err(|_| Error::invalid("arith"))?;
            match r {
                ScalarName::Known(s) => Ok(Type::Scalar(s)),
                _ => Err(Error::invalid("arith")),
            }
        }
        _ => Err(Error::invalid(format!("common type of {a} and {b}"))),
    }
}

fn load(p: Place) -> Expr {
    let ty = p.ty.unpacked();
    Expr::new(ExprKind::Load(Box::new(p)), ty)
}

fn load_local(id: LocalId, ty: Type) -> Expr {
    load(Place {
        kind: PlaceKind::Local(id),
        ty,
        space: Space::Thread,
    })
}

fn assign_local(id: LocalId, v: Expr) -> Stmt {
    let ty = v.ty.clone();
    let p = Place {
        kind: PlaceKind::Local(id),
        ty: ty.clone(),
        space: Space::Thread,
    };
    Stmt::Expr(Expr::new(ExprKind::Assign(Box::new(p), Box::new(v)), ty))
}

fn deref(ptr: Expr) -> R<Place> {
    let Type::Ptr(sp, inner) = &ptr.ty else {
        return Err(Error::invalid("deref of non-pointer"));
    };
    let (sp, ty) = (*sp, (**inner).clone());
    Ok(Place {
        kind: PlaceKind::Deref(Box::new(ptr)),
        ty,
        space: sp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower(src: &str) -> R<Module> {
        let out = msl_parse::check_source(src);
        let msl_parse::Outcome::Checked(c) = out else {
            panic!("parse failed")
        };
        assert!(c.reasons.is_empty(), "rejected: {:?}", c.reasons);
        analyze(&c.module)
    }

    const PRE: &str = "#include <metal_stdlib>\nusing namespace metal;\n";

    #[test]
    fn literals() {
        assert_eq!(parse_int("4294966296u").unwrap(), 4294966296);
        assert_eq!(parse_int("0x10").unwrap(), 16);
        assert_eq!(parse_int("-3").unwrap(), -3);
        assert_eq!(
            parse_float("9.9999997473787516355514526367188e-05").unwrap(),
            f64::from(f32::from_bits(0x38d1_b717))
        );
    }

    #[test]
    fn kernel_with_argument_buffer() {
        let src = format!(
            "{PRE}struct _15 {{ uint _m0[1]; }};\nstruct spvDescriptorSetBuffer0 {{ device _15* cp_s0 [[id(0)]]; }};\n\
             kernel void main0(constant spvDescriptorSetBuffer0& spvDescriptorSet0 [[buffer(0)]])\n{{\n    (*spvDescriptorSet0.cp_s0)._m0[0] = 1000u;\n}}\n"
        );
        let m = lower(&src).unwrap();
        assert_eq!(m.funcs.len(), 1);
        let f = &m.funcs[0];
        assert_eq!(f.params[0].by_ref, Some(Space::Constant));
        assert!(m.types.is_argument_buffer(0) || m.types.is_argument_buffer(1));
        assert_eq!(f.body.len(), 1);
    }

    #[test]
    fn implicit_conversions_are_explicit() {
        let src = format!("{PRE}static float f(float a, int b) {{ return a * b; }}\n");
        let m = lower(&src).unwrap();
        let Stmt::Return(Some(e)) = &m.funcs[0].body[0] else {
            panic!()
        };
        let ExprKind::Binary(BinOp::Mul, _, b) = &e.kind else {
            panic!("{e:?}")
        };
        assert!(matches!(b.kind, ExprKind::Convert(_)));
        assert_eq!(b.ty, Type::FLOAT);
    }

    #[test]
    fn switch_arms_merge_labels() {
        let src = format!(
            "{PRE}static int f(uint x) {{ switch (x) {{ case 1u: case 2u: return 1; default: return 0; }} }}\n"
        );
        let m = lower(&src).unwrap();
        let Stmt::Switch { arms, .. } = &m.funcs[0].body[0] else {
            panic!()
        };
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[0].labels, vec![Some(1), Some(2)]);
        assert_eq!(arms[1].labels, vec![None]);
    }

    #[test]
    fn unknown_builtin_is_unsupported() {
        let src = format!("{PRE}static float f(float a) {{ return tan(a); }}\n");
        assert!(lower(&src).is_ok());
    }
}
