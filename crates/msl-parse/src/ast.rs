//! The syntax tree of an accepted file. Every expression carries the type the checker gave it
//! (including `Ref` for lvalues, as in the prototype) and its source span.
//!
//! The prototype builds no tree: it only type-checks. This tree is what the IR lowering of
//! the next milestone consumes. An item the checker rejects appears as [`Item::Rejected`].

use std::rc::Rc;

use msl_lex::Span;

use crate::ty::Ty;

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub enum Item {
    /// An accepted preprocessor line (`#include <metal_stdlib>` ...).
    Preprocessor {
        text: String,
        span: Span,
    },
    /// `using namespace metal;`
    UsingNamespaceMetal {
        span: Span,
    },
    /// An engine template helper recognised by its text hash.
    Prelude {
        name: &'static str,
        hash: String,
        span: Span,
    },
    Struct(StructDef),
    /// `constant T name[N] = { ... };`
    ConstArray {
        name: Rc<str>,
        ty: Ty,
        init: Vec<Expr>,
        span: Span,
    },
    /// `constant T name [[function_constant(N)]];`
    FunctionConstant {
        name: Rc<str>,
        ty: Ty,
        attrs: Vec<Attr>,
        span: Span,
    },
    Function(Rc<Function>),
    /// An item the checker rejected, with the prototype's reason key and detail.
    Rejected {
        reason: String,
        detail: String,
        span: Span,
    },
}

/// An attribute `[[name(args)]]`: the name (with `ns::` if qualified) and the argument token texts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    pub name: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: Rc<str>,
    pub fields: Vec<Field>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: Rc<str>,
    pub ty: Ty,
    pub attrs: Vec<Attr>,
    /// The declaration had `const` (for example `device const uchar* base`).
    pub is_const: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
    Kernel,
}

impl Stage {
    pub fn from_name(s: &str) -> Option<Stage> {
        Some(match s {
            "vertex" => Stage::Vertex,
            "fragment" => Stage::Fragment,
            "kernel" => Stage::Kernel,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Rc<str>,
    pub ty: Ty,
    pub attrs: Vec<Attr>,
    /// The declaration had `const`.
    pub is_const: bool,
}

/// A function signature. The body is in [`Function::body`] once parsed.
#[derive(Clone, Debug)]
pub struct Signature {
    pub name: Rc<str>,
    pub entry: Option<Stage>,
    pub ret: Ty,
    pub params: Vec<Param>,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub sig: Rc<Signature>,
    /// `[[early_fragment_tests]]` and the like, before the declaration.
    pub attrs: Vec<Attr>,
    /// `static` or `inline` was given.
    pub is_static: bool,
    /// `inline` was given (clang marks such functions `inlinehint`).
    pub is_inline: bool,
    /// `__attribute__((always_inline))` was given.
    pub always_inline: bool,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// The block starts with `#pragma clang fp contract(off)`.
    pub contract_off: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Block(Block),
    Empty,
    If {
        cond: Box<Expr>,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    Switch {
        cond: Box<Expr>,
        body: Vec<SwitchEntry>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Box<Expr>,
    },
    While {
        cond: Box<Expr>,
        body: Box<Stmt>,
    },
    For {
        init: Option<ForInit>,
        cond: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
        body: Box<Stmt>,
    },
    Break,
    Continue,
    Return(Option<Box<Expr>>),
    DiscardFragment,
    Decl(Decl),
    Expr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum ForInit {
    Decl(Decl),
    Expr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum SwitchEntry {
    /// `case N:` with the literal's text.
    Case(String),
    Default,
    Stmt(Stmt),
}

#[derive(Clone, Debug)]
pub struct Decl {
    pub ty: Ty,
    pub vars: Vec<DeclVar>,
    /// Static workgroup storage, not a private stack allocation.
    pub threadgroup: bool,
}

#[derive(Clone, Debug)]
pub struct DeclVar {
    pub name: Rc<str>,
    /// The declared type including array dimensions.
    pub ty: Ty,
    pub init: Init,
}

#[derive(Clone, Debug)]
pub enum Init {
    None,
    /// `= {}`
    Zero,
    Expr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    LogOr,
    LogAnd,
    BitOr,
    BitXor,
    BitAnd,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinOp {
    pub fn from_op(s: &str) -> Option<BinOp> {
        Some(match s {
            "||" => BinOp::LogOr,
            "&&" => BinOp::LogAnd,
            "|" => BinOp::BitOr,
            "^" => BinOp::BitXor,
            "&" => BinOp::BitAnd,
            "==" => BinOp::Eq,
            "!=" => BinOp::Ne,
            "<" => BinOp::Lt,
            ">" => BinOp::Gt,
            "<=" => BinOp::Le,
            ">=" => BinOp::Ge,
            "<<" => BinOp::Shl,
            ">>" => BinOp::Shr,
            "+" => BinOp::Add,
            "-" => BinOp::Sub,
            "*" => BinOp::Mul,
            "/" => BinOp::Div,
            "%" => BinOp::Rem,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastKind {
    /// `(T)x`
    CStyle,
    /// `as_type<T>(x)`
    AsType,
    /// `static_cast<T>(x)`
    Static,
    /// `reinterpret_cast<T*>(x)` from a pointer or a 64-bit integer.
    Reinterpret,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarScope {
    Local,
    Global,
}

/// What a call calls.
#[derive(Clone, Debug)]
pub enum Callee {
    /// A user function, resolved to the overload the checker picked.
    User(Rc<Signature>),
    /// An engine template helper (`dm_bs`, `spvDepthCast`, `spvFAdd`).
    Prelude(&'static str),
    /// A metal_stdlib function or sampling option (`fma`, `precise::clamp`, `level` ...).
    Builtin(Rc<str>),
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    /// A numeric literal with its source text.
    Literal(Rc<str>),
    Bool(bool),
    Var {
        name: Rc<str>,
        scope: VarScope,
    },
    Unary {
        op: UnOp,
        operand: Box<Expr>,
    },
    Deref(Box<Expr>),
    AddrOf(Box<Expr>),
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `op` is None for `=`, else the operator of a compound assignment.
    Assign {
        op: Option<BinOp>,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Cond {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// `base.field`, or `base->field` with `arrow`.
    Member {
        base: Box<Expr>,
        field: Rc<str>,
        arrow: bool,
    },
    /// `base.xyz`: component indices 0 to 3.
    Swizzle {
        base: Box<Expr>,
        comps: Vec<u8>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Call {
        callee: Callee,
        args: Vec<Expr>,
    },
    /// `texture.method(args)`
    Method {
        base: Box<Expr>,
        name: Rc<str>,
        args: Vec<Expr>,
    },
    /// `float4(...)`, `uint(x)`: the type is the expression's type.
    Construct {
        args: Vec<Expr>,
    },
    Cast {
        kind: CastKind,
        to: Ty,
        operand: Box<Expr>,
    },
    /// `S{a, b}`
    StructInit {
        name: Rc<str>,
        args: Vec<Expr>,
    },
    /// `component::x`
    Component(Rc<str>),
    /// A parenthesised expression.
    Paren(Box<Expr>),
}
