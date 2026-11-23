//! Syntax tree to [`hir`]: scopes, places, conversions, overloads, helper expansion.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use msl_parse::ast::{self, Callee, CastKind, ExprKind as A, Init, Item, SwitchEntry, VarScope};
use msl_parse::ty::{arith, ScalarName, Ty};

use crate::hir::{
    Arg, BinOp, CmpOp, ConstArray, Expr, ExprKind, FnConst, Func, Local, LocalId, Module, Param,
    Place, PlaceKind, Query, SampleOpt, Stmt, SwitchArm, TexOp, TextureAtomicFetch, UnOp,
};
use crate::types::{is_float, is_signed, Access, Scalar, Space, Tex, TexKind, Type};
use crate::{builtins, conv, Error};

mod pool;

type R<T> = Result<T, Error>;

/// A dm_bs instantiation: argument types, sampling option per extra argument, function.
type BsInstance = (Vec<Type>, Vec<Option<OptKind>>, usize);

#[derive(Clone, Copy, PartialEq, Eq)]
enum OptKind {
    Level,
    Bias,
    Grad2d,
    Grad3d,
    GradCube,
}

impl OptKind {
    fn named(name: &str) -> Option<Self> {
        match name {
            "level" => Some(Self::Level),
            "bias" => Some(Self::Bias),
            "gradient2d" => Some(Self::Grad2d),
            "gradient3d" => Some(Self::Grad3d),
            "gradientcube" => Some(Self::GradCube),
            _ => None,
        }
    }

    fn arity(self) -> usize {
        match self {
            Self::Grad2d | Self::Grad3d | Self::GradCube => 2,
            _ => 1,
        }
    }

    fn lower(self, previous: &SampleOpt, mut value: impl FnMut(usize) -> R<Expr>) -> R<SampleOpt> {
        let repeated = match (previous, self) {
            (SampleOpt::Grad2d(..), _) => Some("gradient2d"),
            (SampleOpt::Grad3d(..), _) => Some("gradient3d"),
            (SampleOpt::GradCube(..), _) => Some("gradientcube"),
            (SampleOpt::None, _) => None,
            (_, Self::Grad2d) => Some("gradient2d"),
            (_, Self::Grad3d) => Some("gradient3d"),
            (_, Self::GradCube) => Some("gradientcube"),
            _ => None,
        };
        if let Some(kind) = repeated {
            return Err(Error::invalid(format!(
                "multiple sample options with {kind}"
            )));
        }
        Ok(match self {
            Self::Level => SampleOpt::Level(Box::new(value(0)?)),
            Self::Bias => SampleOpt::Bias(Box::new(value(0)?)),
            Self::Grad2d => SampleOpt::Grad2d(Box::new(value(0)?), Box::new(value(1)?)),
            Self::Grad3d => SampleOpt::Grad3d(Box::new(value(0)?), Box::new(value(1)?)),
            Self::GradCube => SampleOpt::GradCube(Box::new(value(0)?), Box::new(value(1)?)),
        })
    }

    fn operand(self, value: Expr) -> R<Expr> {
        match self {
            Self::Grad2d if value.ty != Type::Vector(Scalar::Float, 2) => {
                Err(Error::invalid("gradient2d requires float2 derivatives"))
            }
            Self::Grad3d if value.ty != Type::Vector(Scalar::Float, 3) => {
                Err(Error::invalid("gradient3d requires float3 derivatives"))
            }
            Self::GradCube if value.ty != Type::Vector(Scalar::Float, 3) => {
                Err(Error::invalid("gradientcube requires float3 derivatives"))
            }
            Self::Grad2d | Self::Grad3d | Self::GradCube => Ok(value),
            _ => conv(value, &Type::FLOAT),
        }
    }
}

fn array_integer_bounds(s: Scalar) -> (i128, i128) {
    if s == Scalar::Bool {
        return (0, 1);
    }
    let signed = is_signed(s);
    let limit = 1i128 << (s.size() * 8 - u32::from(signed));
    (if signed { -limit } else { 0 }, limit - 1)
}

// Keep the same exact f32 subset as the emitter's array initializer fold.
fn scalar_array_float_value(value: &Expr) -> Option<f64> {
    if value.ty != Type::FLOAT {
        return None;
    }
    let f = match &value.kind {
        ExprKind::Float(f) => *f,
        ExprKind::Unary(UnOp::Neg, e) => -scalar_array_float_value(e)?,
        ExprKind::Convert(e) => {
            let n = scalar_array_integer_value(e)?;
            let f = n as f32;
            if f as i128 != n {
                return None;
            }
            f64::from(f)
        }
        _ => return None,
    };
    f.is_finite().then_some(f)
}

// Recover only integer values that the existing emitter can safely fold.
fn scalar_array_integer_value(value: &Expr) -> Option<i128> {
    let Type::Scalar(ty) = value.ty else {
        return None;
    };
    if is_float(ty) {
        return None;
    }
    let n = match &value.kind {
        ExprKind::Int(n) => *n,
        ExprKind::Unary(UnOp::Neg, e) if is_signed(ty) => match e.kind {
            ExprKind::Int(n) => n.checked_neg()?,
            _ => scalar_array_integer_value(e)?.checked_neg()?,
        },
        ExprKind::Convert(e) => {
            let n = if e.ty == Type::FLOAT {
                let f = scalar_array_float_value(e)?;
                if ty == Scalar::Bool {
                    return Some(i128::from(f != 0.0));
                }
                f.trunc() as i128
            } else {
                scalar_array_integer_value(e)?
            };
            if ty == Scalar::Bool {
                i128::from(n != 0)
            } else {
                n
            }
        }
        _ => return None,
    };
    let (min, max) = array_integer_bounds(ty);
    (min..=max).contains(&n).then_some(n)
}

/// Check the initializer before inserting its implicit conversion.
/// A constructor already has its explicit result type at this point.
fn check_array_initializer(value: &Expr, dest: &Type) -> R<()> {
    if matches!(
        (&value.ty, dest),
        (Type::Vector(from, a), Type::Vector(to, b)) if a == b && from != to
    ) {
        return Err(Error::unsupported("const-array:implicit-narrowing"));
    }
    let (Type::Scalar(from), Type::Scalar(to)) = (&value.ty, dest) else {
        return Ok(());
    };
    if let ExprKind::Int(n) = &value.kind {
        let (min, max) = array_integer_bounds(*from);
        if !(min..=max).contains(n) {
            return Err(Error::unsupported("const-array:integer-literal-range"));
        }
    }
    let literal = scalar_array_integer_value(value);
    let non_narrowing = if from == to {
        true
    } else if is_float(*from) {
        is_float(*to)
    } else if !is_float(*to) {
        let (min, max) = array_integer_bounds(*to);
        let (from_min, from_max) = array_integer_bounds(*from);
        (min <= from_min && from_max <= max) || literal.is_some_and(|n| (min..=max).contains(&n))
    } else if *from == Scalar::Bool {
        true
    } else {
        literal.is_some_and(|n| {
            if *to == Scalar::Float {
                return (n as f32) as i128 == n;
            }
            // Half has 11 significant bits, including the leading one.
            let magnitude = n.unsigned_abs();
            magnitude == 0
                || (magnitude <= 65_504
                    && magnitude.trailing_zeros() >= magnitude.ilog2().saturating_sub(10))
        })
    };
    if non_narrowing {
        Ok(())
    } else {
        Err(Error::unsupported("const-array:implicit-narrowing"))
    }
}

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
    volatile_structs: HashSet<usize>,
    fconst_ids: HashMap<Rc<str>, usize>,
    const_ids: HashMap<Rc<str>, usize>,
    /// Signatures of the lowered functions, to resolve `Callee::User` by pointer.
    sigs: Vec<(Rc<ast::Signature>, usize)>,
    preludes: Vec<&'static str>,
    /// A file-scope `#pragma clang fp contract(off)` came before the current item.
    contract_off: bool,
    barrier_funcs: HashSet<usize>,
    simd_barrier_funcs: HashSet<usize>,
    fragment_lod_funcs: HashMap<usize, &'static str>,
    // per function
    uses_threadgroup_barrier: bool,
    uses_simdgroup_barrier: bool,
    fragment_lod_error: Option<&'static str>,
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
            Ty::ThreadgroupAtomicUintPtr => {
                Type::Ptr(Space::Threadgroup, Rc::new(Type::Scalar(Scalar::Uint)))
            }
            Ty::DeviceAtomicUintPtr => Type::Ptr(Space::Device, Rc::new(Type::UINT)),
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
                for (k, field) in s.fields.iter().enumerate() {
                    self.out.types.structs[id].fields[k].is_volatile = field.is_volatile;
                }
                if self.out.types.structs[id]
                    .fields
                    .iter()
                    .any(|f| f.is_volatile || self.has_volatile_field(&f.ty))
                {
                    self.volatile_structs.insert(id);
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
                if init.len() > *n as usize {
                    return Err(Error::invalid("const-array:excess-init"));
                }
                if init.len() < *n as usize {
                    if !matches!(el.as_ref(), Type::Scalar(_) | Type::Vector(_, _)) {
                        return Err(Error::unsupported("const-array:partial-element-type"));
                    }
                    // A short declaration must not create an arbitrarily large GPU object.
                    const MAX_PARTIAL_BYTES: u32 = 16 * 1024 * 1024;
                    if *n > MAX_PARTIAL_BYTES / self.out.types.layout(el).size {
                        return Err(Error::unsupported("const-array:partial-size"));
                    }
                }
                let mut vals = Vec::with_capacity(init.len());
                for e in init {
                    self.locals.clear();
                    let v = self.expr(e)?;
                    check_array_initializer(&v, el)?;
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
        self.uses_threadgroup_barrier = false;
        self.uses_simdgroup_barrier = false;
        self.fragment_lod_error = None;
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
        if self.uses_threadgroup_barrier {
            if sig.entry == Some(ast::Stage::Vertex) {
                return Err(Error::unsupported("threadgroup-barrier:vertex"));
            }
            self.barrier_funcs.insert(self.out.funcs.len());
        }
        if self.uses_simdgroup_barrier {
            if matches!(sig.entry, Some(ast::Stage::Vertex | ast::Stage::Fragment)) {
                return Err(Error::unsupported("simdgroup-barrier:noncompute"));
            }
            self.simd_barrier_funcs.insert(self.out.funcs.len());
        }
        if let Some(error) = self.fragment_lod_error {
            if matches!(sig.entry, Some(ast::Stage::Vertex | ast::Stage::Kernel)) {
                return Err(Error::unsupported(error));
            }
            self.fragment_lod_funcs.insert(self.out.funcs.len(), error);
        }
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
        self.locals.push(Local {
            name,
            ty,
            threadgroup: false,
        });
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
            ast::Stmt::SharedHalfStore { base, index, value } => {
                let base = self.expr(base)?;
                if base.ty != Type::Ptr(Space::Threadgroup, Rc::new(Type::UINT)) {
                    return Err(Error::unsupported("shared-half-store:base"));
                }
                let half = Type::Scalar(Scalar::Ushort);
                let value = self.expr(value)?;
                let offset = self.index(index)?;
                if value.ty != half || offset.ty != Type::UINT {
                    return Err(Error::unsupported("shared-half-store:operands"));
                }
                let ptr_ty = Type::Ptr(Space::Threadgroup, Rc::new(half));
                let base = Expr::new(ExprKind::PtrCast(Box::new(base)), ptr_ty.clone());
                let place = deref(Expr::new(
                    ExprKind::PtrOffset(Box::new(base), Box::new(offset)),
                    ptr_ty,
                ))?;
                out.push(Stmt::MayAliasStore { place, value });
            }
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
            self.locals[id].threadgroup = d.threadgroup;
            if !d.threadgroup {
                out.push(Stmt::Decl(id));
            }
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
            A::Cond { .. } => matches!(e.ty, Ty::Ref(Space::Thread, _)),
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
            A::Cond { cond, then, els } if matches!(e.ty, Ty::Ref(Space::Thread, _)) => {
                let c = self.cond(cond)?;
                let yes = self.place(then)?;
                let no = self.place(els)?;
                if !matches!(yes.ty, Type::Vector(..))
                    || yes.ty != no.ty
                    || yes.space != Space::Thread
                    || no.space != Space::Thread
                {
                    return Err(Error::unsupported("conditional-field-place"));
                }
                let ptr_ty = Type::Ptr(Space::Thread, Rc::new(yes.ty.clone()));
                let yes = Expr::new(ExprKind::AddrOf(Box::new(yes)), ptr_ty.clone());
                let no = Expr::new(ExprKind::AddrOf(Box::new(no)), ptr_ty.clone());
                deref(Expr::new(
                    ExprKind::Cond(Box::new(c), Box::new(yes), Box::new(no)),
                    ptr_ty,
                ))
            }

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
                    let local = &self.locals[id];
                    Ok(Place {
                        kind: PlaceKind::Local(id),
                        ty: local.ty.clone(),
                        space: if local.threadgroup {
                            Space::Threadgroup
                        } else {
                            Space::Thread
                        },
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
            A::Deref(p) => self.deref_ast(p),
            A::Member { base, field, arrow } => {
                let bp = if *arrow {
                    self.deref_ast(base)?
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

    fn deref_ast(&mut self, e: &ast::Expr) -> R<Place> {
        match &e.kind {
            A::Paren(p) => self.deref_ast(p),
            A::Member { .. } if self.is_place(e) => {
                let p = self.place(e)?;
                if self.is_volatile_field(&p) {
                    if p.space != Space::Constant {
                        return Err(Error::unsupported("volatile-field:binding"));
                    }
                    let mut target = deref(load(p))?;
                    let PlaceKind::Deref(ptr) = target.kind else {
                        return Err(Error::invalid("volatile resource deref"));
                    };
                    target.kind = PlaceKind::VolatileDeref(ptr);
                    Ok(target)
                } else {
                    deref(self.read_place(p)?)
                }
            }
            _ => deref(self.expr(e)?),
        }
    }

    fn is_volatile_field(&self, p: &Place) -> bool {
        let PlaceKind::Field(base, index) = &p.kind else {
            return false;
        };
        let Type::Struct(id) = base.ty else {
            return false;
        };
        self.out.types.structs[id].fields[*index as usize].is_volatile
    }

    fn has_volatile_field(&self, ty: &Type) -> bool {
        match ty {
            Type::Struct(id) => self.volatile_structs.contains(id),
            Type::Array(elem, _) => self.has_volatile_field(elem),
            _ => false,
        }
    }

    fn read_place(&self, p: Place) -> R<Expr> {
        if self.is_volatile_field(&p) || self.has_volatile_field(&p.ty) {
            return Err(Error::unsupported("volatile-field:pointer-escape"));
        }
        if p.is_volatile()
            && (!matches!(p.ty, Type::Scalar(_))
                || matches!(p.kind, PlaceKind::Element(..) | PlaceKind::Swizzle(..)))
        {
            return Err(Error::unsupported("volatile-field:non-scalar"));
        }
        Ok(load(p))
    }

    fn check_unqualified_place(&self, p: &Place) -> R<()> {
        if p.is_volatile() || self.is_volatile_field(p) || self.has_volatile_field(&p.ty) {
            return Err(Error::unsupported("volatile-field:write-or-escape"));
        }
        Ok(())
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
                    return self.read_place(p);
                }
                let p = self.place(e)?;
                self.read_place(p)
            }
            A::Swizzle { base, comps } => {
                if self.is_place(e) {
                    let p = self.place(e)?;
                    return self.read_place(p);
                }
                let b = self.expr(base)?;
                Ok(Expr::new(ExprKind::Swizzle(Box::new(b), comps.clone()), ty))
            }
            A::Paren(x) => self.expr(x),
            A::AddrOf(x) => {
                let p = self.place(x)?;
                self.check_unqualified_place(&p)?;
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
                self.check_unqualified_place(&p)?;
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
            A::Cast { kind, to, operand } => {
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
                    // A device atomic Uint word has the same AIR pointer type as a device uint.
                    CastKind::Reinterpret if *to == Ty::DeviceAtomicUintPtr && v.ty == ty => Ok(v),
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
                self.uses_threadgroup_barrier |= self.barrier_funcs.contains(&fid);
                self.uses_simdgroup_barrier |= self.simd_barrier_funcs.contains(&fid);
                self.fragment_lod_error = self
                    .fragment_lod_error
                    .or_else(|| self.fragment_lod_funcs.get(&fid).copied());
                let mut out = Vec::with_capacity(args.len());
                let params: Vec<(Option<Space>, Type)> = self.out.funcs[fid]
                    .params
                    .iter()
                    .map(|p| (p.by_ref, p.ty.clone()))
                    .collect();
                for (a, (by_ref, pt)) in args.iter().zip(params.iter()) {
                    if by_ref.is_some() {
                        let p = self.place(a)?;
                        self.check_unqualified_place(&p)?;
                        if (*by_ref == Some(Space::Threadgroup) || p.space == Space::Threadgroup)
                            && (*by_ref != Some(p.space) || !self.is_place(a))
                        {
                            return Err(Error::invalid(
                                "threadgroup reference address space mismatch",
                            ));
                        }
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
                self.uses_threadgroup_barrier |= name.as_ref() == "threadgroup_barrier";
                self.uses_simdgroup_barrier |= name.as_ref() == "simdgroup_barrier";
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
        // Flatten the option's fields, but retain its kind in the template specialization.
        let mut opts = Vec::new();
        for a in &args[5..] {
            if let A::Call {
                callee: Callee::Builtin(n),
                args: oa,
            } = &a.kind
            {
                if let Some(opt) = OptKind::named(n) {
                    for operand in oa {
                        vals.push(opt.operand(self.expr(operand)?)?);
                    }
                    opts.push(Some(opt));
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
        opts: &[Option<OptKind>],
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
        let mut k = 5;
        let extra: Vec<_> = opts
            .iter()
            .map(|opt| {
                let start = k;
                k += opt.map_or(1, OptKind::arity);
                match opt {
                    Some(o) => SampleArg::Opt(*o, start),
                    None => SampleArg::Local(start),
                }
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
        let buffer = tx.kind == TexKind::Buffer;
        if buffer
            && !match name {
                "read" => args.len() == 1,
                "write"
                    if tx.access == Access::ReadWrite
                        || (tx.elem == Scalar::Uint && tx.access == Access::Write) =>
                {
                    args.len() == 2
                }
                "get_width" if tx.access == Access::Read => args.is_empty(),
                _ => false,
            }
        {
            return Err(Error::unsupported("texture_buffer:read-index"));
        }
        let ms_array = tx.kind == TexKind::T2dMsArray;
        if ms_array
            && (!args.is_empty()
                || !matches!(
                    name,
                    "get_width" | "get_height" | "get_array_size" | "get_num_samples"
                ))
        {
            return Err(Error::unsupported("texture2d_ms_array:query"));
        }
        let multisampled = tx.kind == TexKind::T2dMs;
        if multisampled
            && !match name {
                "read" => args.len() == 2,
                "get_width" | "get_height" | "get_num_samples" => args.is_empty(),
                _ => false,
            }
        {
            return Err(Error::unsupported("texture2d_ms:read-coord-sample"));
        }
        let tex_b = Box::new(tex.clone());
        let lod_arg = |cx: &mut Cx, a: Option<&ast::Expr>| -> R<Option<Box<Expr>>> {
            match a {
                Some(a) => Ok(Some(Box::new(conv(cx.expr(a)?, &Type::UINT)?))),
                None => Ok(None),
            }
        };
        let op = match name {
            "sample" | "sample_compare" | "gather" | "gather_compare" => {
                let extra: Vec<SampleArg> = args.iter().map(SampleArg::Ast).collect();
                let e = self.sample_op(tx, tex, name, &extra)?;
                return Ok(Expr { ty, ..e });
            }
            "read" => {
                let dim = tx.kind.coord_dim();
                let coord = self.expr(&args[0])?;
                let coord_ty = Type::vec(Scalar::Uint, dim);
                if buffer && coord.ty != Type::UINT {
                    return Err(Error::unsupported("texture_buffer:read-index"));
                }
                if multisampled && coord.ty != coord_ty {
                    return Err(Error::invalid(
                        "multisample read requires uint2 coordinates",
                    ));
                }
                let coord = conv(coord, &coord_ty)?;
                let mut rest = &args[1..];
                let array = if tx.kind.is_array() {
                    let a = rest.first().ok_or_else(|| Error::invalid("read slice"))?;
                    rest = &rest[1..];
                    Some(Box::new(conv(self.expr(a)?, &Type::UINT)?))
                } else {
                    None
                };
                if tx.kind == TexKind::T3d && rest.len() > 1 {
                    return Err(Error::unsupported("read:texture3d-args"));
                }
                let lod = if multisampled {
                    let sample = self.expr(&args[1])?;
                    if sample.ty != Type::UINT {
                        return Err(Error::invalid(
                            "multisample read requires a uint sample index",
                        ));
                    }
                    Some(Box::new(sample))
                } else {
                    lod_arg(self, rest.first())?
                };
                TexOp::Read {
                    coord: Box::new(coord),
                    array,
                    lod,
                }
            }
            "write" if tx.kind == TexKind::T2dArray => {
                let unsupported = || Error::unsupported("write:texture2d_array");
                let [value, coord, slice] = args else {
                    return Err(unsupported());
                };
                if tx.elem != Scalar::Float || tx.access != Access::Write {
                    return Err(unsupported());
                }
                let value = self.expr(value)?;
                let coord = self.expr(coord)?;
                let slice = self.expr(slice)?;
                if value.ty != Type::Vector(Scalar::Float, 4)
                    || coord.ty != Type::Vector(Scalar::Uint, 2)
                    || slice.ty != Type::UINT
                {
                    return Err(unsupported());
                }
                TexOp::Write {
                    value: Box::new(value),
                    coord: Box::new(coord),
                    array: Some(Box::new(slice)),
                    lod: None,
                }
            }
            "write" => {
                let value = self.expr(&args[0])?;
                let value_ty = Type::Vector(tx.elem, 4);
                if buffer && value.ty != value_ty {
                    return Err(Error::unsupported("texture_buffer:write-value"));
                }
                let value = conv(value, &value_ty)?;
                let dim = tx.kind.coord_dim();
                let coord = self.expr(&args[1])?;
                if buffer && coord.ty != Type::UINT {
                    return Err(Error::unsupported("texture_buffer:write-index"));
                }
                let coord = conv(coord, &Type::vec(Scalar::Uint, dim))?;
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
            "atomic_fetch_or" | "atomic_fetch_add" | "atomic_fetch_max" | "atomic_fetch_min" => {
                let [coord, value] = args else {
                    return Err(Error::unsupported(format!("method:{name}")));
                };
                let coord = self.expr(coord)?;
                let value = self.expr(value)?;
                let supported_element = matches!(
                    (name, tx.elem),
                    (
                        "atomic_fetch_or" | "atomic_fetch_add" | "atomic_fetch_max",
                        Scalar::Uint
                    ) | ("atomic_fetch_max" | "atomic_fetch_min", Scalar::Int)
                );
                if tx.kind != TexKind::T2d
                    || !supported_element
                    || tx.access != Access::ReadWrite
                    || coord.ty != Type::Vector(Scalar::Uint, 2)
                    || value.ty != Type::Vector(tx.elem, 4)
                {
                    return Err(Error::unsupported(format!("method:{name}")));
                }
                TexOp::AtomicFetch {
                    op: match name {
                        "atomic_fetch_or" => TextureAtomicFetch::Or,
                        "atomic_fetch_add" => TextureAtomicFetch::Add,
                        "atomic_fetch_max" => TextureAtomicFetch::Max,
                        "atomic_fetch_min" => TextureAtomicFetch::Min,
                        _ => unreachable!(),
                    },
                    coord: Box::new(coord),
                    value: Box::new(value),
                }
            }
            "get_num_samples" if multisampled || ms_array => TexOp::Query {
                what: Query::Samples,
                lod: None,
            },
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
                self.fragment_lod_error = self.fragment_lod_error.or(match tx.kind {
                    TexKind::T3d => Some("lod:texture3d-stage"),
                    TexKind::Cube => Some("lod:texturecube-stage"),
                    TexKind::CubeArray => Some("lod:texturecube_array-stage"),
                    _ => None,
                });
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

    /// Sampling and gather methods take (sampler, coord, [slice], [ref], options...).
    fn sample_op(&mut self, tx: Tex, tex: Expr, name: &str, args: &[SampleArg<'_>]) -> R<Expr> {
        if tx.kind == TexKind::T2dMsArray {
            return Err(Error::unsupported("texture2d_ms_array:query"));
        }
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
        let compare_gather = name == "gather_compare";
        if compare_gather && tx.access != Access::Sample {
            return Err(Error::unsupported("gather_compare:access"));
        }
        let compare = if compare_gather || name == "sample_compare" {
            let r = self.sample_arg(it.next().ok_or_else(|| Error::invalid("compare ref"))?)?;
            Some(Box::new(conv(r, &Type::FLOAT)?))
        } else {
            None
        };
        let mut opt = SampleOpt::None;
        let mut offset = None;
        let mut gather = if compare_gather || name == "gather" {
            Some(0u8)
        } else {
            None
        };
        for a in it {
            if let SampleArg::Opt(o, l) = a {
                opt = o.lower(&opt, |i| {
                    Ok(load_local(l + i, self.locals[l + i].ty.clone()))
                })?;
                continue;
            }
            if let SampleArg::Ast(e) = a {
                match &e.kind {
                    A::Call {
                        callee: Callee::Builtin(n),
                        args: oargs,
                    } => {
                        let o = OptKind::named(n)
                            .ok_or_else(|| Error::unsupported(format!("sample-option:{n}")))?;
                        opt = o.lower(&opt, |i| o.operand(self.expr(&oargs[i])?))?;
                        continue;
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
        if matches!(opt, SampleOpt::Grad2d(..))
            && (name != "sample"
                || tx.elem != Scalar::Float
                || !match tx.kind {
                    TexKind::T2dArray => true,
                    TexKind::T2d => tx.access == Access::Sample && offset.is_none(),
                    _ => false,
                })
        {
            return Err(Error::unsupported("sample-option:gradient2d"));
        }
        if matches!(opt, SampleOpt::Grad3d(..))
            && (name != "sample"
                || tx.kind != TexKind::T3d
                || tx.elem != Scalar::Float
                || tx.access != Access::Sample
                || offset.is_some())
        {
            return Err(Error::unsupported("sample-option:gradient3d"));
        }
        if matches!(opt, SampleOpt::GradCube(..))
            && (name != "sample"
                || !matches!(tx.kind, TexKind::Cube | TexKind::CubeArray)
                || tx.elem != Scalar::Float
                || tx.access != Access::Sample
                || gather.is_some())
        {
            return Err(Error::unsupported("sample-option:gradientcube"));
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
    /// A sampling option whose fields start at this local.
    Opt(OptKind, LocalId),
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
