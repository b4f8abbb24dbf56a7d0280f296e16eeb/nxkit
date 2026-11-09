//! The engine's descriptor-pool helper `dm_pool_mark`, instantiated per pool type:
//!
//! ```text
//! template<typename P> static inline __attribute__((always_inline)) uint dm_pool_mark(constant P* p, uint i)
//! {
//!     device atomic_uint* w = reinterpret_cast<device atomic_uint*>(p->seen) + (i >> 5);
//!     uint b = 1u << (i & 31u);
//!     if (!(atomic_load_explicit(w, memory_order_relaxed) & b)) atomic_fetch_or_explicit(w, b, memory_order_relaxed);
//!     return i;
//! }
//! ```

use std::rc::Rc;

use msl_parse::ast;

use super::{assign_local, deref, load, load_local, Cx, R};
use crate::hir::{Arg, BinOp, Expr, ExprKind, Func, Inline, Param, Stmt, UnOp};
use crate::types::{Scalar, Space, Type};
use crate::{conv, Error};

/// `device atomic_uint*`: an atomic word is a `uint` in device memory.
fn word_ptr() -> Type {
    Type::Ptr(Space::Device, Rc::new(Type::UINT))
}

fn bin(op: BinOp, a: Expr, b: Expr) -> Expr {
    let ty = a.ty.clone();
    Expr::new(ExprKind::Binary(op, Box::new(a), Box::new(b)), ty)
}

impl Cx {
    /// A call of `dm_pool_mark(p, i)`: the arguments are evaluated at the call, the body is the
    /// always-inline instantiation for the pool type.
    pub(super) fn pool_mark(&mut self, args: &[ast::Expr]) -> R<Expr> {
        let [p, i] = args else {
            return Err(Error::invalid("dm_pool_mark arity"));
        };
        let p = self.expr(p)?;
        let i = conv(self.expr(i)?, &Type::UINT)?;
        let fid = match self.pool_inst.iter().find(|(t, _)| *t == p.ty) {
            Some((_, f)) => *f,
            None => {
                let f = self.pool_instantiate(&p.ty)?;
                self.pool_inst.push((p.ty.clone(), f));
                f
            }
        };
        let args = vec![Arg::Value(p), Arg::Value(i)];
        Ok(Expr::new(ExprKind::Call(fid, args), Type::UINT))
    }

    fn pool_instantiate(&mut self, pt: &Type) -> R<usize> {
        let saved = (
            std::mem::take(&mut self.locals),
            std::mem::take(&mut self.ref_locals),
            std::mem::take(&mut self.scopes),
            std::mem::replace(&mut self.ret, Type::UINT),
        );
        let r = self.pool_body(pt);
        let locals = std::mem::take(&mut self.locals);
        (self.locals, self.ref_locals, self.scopes, self.ret) = saved;
        let (params, body) = r?;
        self.out.funcs.push(Func {
            name: "dm_pool_mark".into(),
            entry: None,
            ret: Type::UINT,
            params,
            locals,
            body,
            attrs: Vec::new(),
            inline: Inline::Always,
            template: true,
            contract: !self.contract_off,
        });
        Ok(self.out.funcs.len() - 1)
    }

    fn pool_body(&mut self, pt: &Type) -> R<(Vec<Param>, Vec<Stmt>)> {
        let mut params = Vec::new();
        for (name, ty) in [("p", pt.clone()), ("i", Type::UINT)] {
            let name: Rc<str> = name.into();
            let local = self.new_local(name.clone(), ty.clone());
            params.push(Param {
                name,
                ty,
                by_ref: None,
                local,
                attrs: Vec::new(),
                is_const: false,
            });
        }
        let (pl, il) = (params[0].local, params[1].local);
        let i = || load_local(il, Type::UINT);
        let uint = |v| Expr::int(v, Type::UINT);
        let seen = load(self.field(deref(load_local(pl, pt.clone()))?, "seen")?);
        if !matches!(seen.ty, Type::Scalar(Scalar::Ulong | Scalar::Long)) {
            return Err(Error::unsupported("dm_pool_mark:seen"));
        }
        let base = Expr::new(ExprKind::PtrCast(Box::new(seen)), word_ptr());
        let w_init = Expr::new(
            ExprKind::PtrOffset(Box::new(base), Box::new(bin(BinOp::Shr, i(), uint(5)))),
            word_ptr(),
        );
        let b_init = bin(BinOp::Shl, uint(1), bin(BinOp::And, i(), uint(31)));
        let w = self.new_local("w".into(), word_ptr());
        let b = self.new_local("b".into(), Type::UINT);
        let word = || load_local(w, word_ptr());
        let bit = || load_local(b, Type::UINT);
        let old = Expr::new(
            ExprKind::Builtin("atomic_load_explicit", vec![word()]),
            Type::UINT,
        );
        let set = conv(bin(BinOp::And, old, bit()), &Type::BOOL)?;
        let cond = Expr::new(ExprKind::Unary(UnOp::Not, Box::new(set)), Type::BOOL);
        let or = Expr::new(
            ExprKind::Builtin("atomic_fetch_or_explicit", vec![word(), bit()]),
            Type::UINT,
        );
        let body = vec![
            Stmt::Decl(w),
            assign_local(w, w_init),
            Stmt::Decl(b),
            assign_local(b, b_init),
            Stmt::If {
                cond,
                then: vec![Stmt::Expr(or)],
                els: Vec::new(),
            },
            Stmt::Return(Some(i())),
        ];
        Ok((params, body))
    }
}
