//! The resolved program the emitter consumes: every name is resolved to a local, a global or a
//! function, every implicit conversion is explicit, every builtin call has its overload.

use std::rc::Rc;

use msl_parse::ast::{Attr, Stage};

use crate::types::{Space, StructDef, Tex, Type, Types};

/// Index of a local variable in [`Func::locals`].
pub type LocalId = usize;
/// Index of a function in [`Module::funcs`].
pub type FuncId = usize;
/// Index of a function constant in [`Module::fconsts`].
pub type FcId = usize;
/// Index of a constant array in [`Module::consts`].
pub type ConstId = usize;

/// A whole translation unit.
#[derive(Clone, Debug, Default)]
pub struct Module {
    /// Types and struct layouts.
    pub types: Types,
    /// `constant T name[N] = {...};`
    pub consts: Vec<ConstArray>,
    /// `constant T name [[function_constant(i)]];`
    pub fconsts: Vec<FnConst>,
    /// User functions and entry points, in source order.
    pub funcs: Vec<Func>,
}

impl Module {
    /// The struct definition of an id.
    pub fn struct_def(&self, id: usize) -> &StructDef {
        &self.types.structs[id]
    }
}

/// A global constant array with its initialiser.
#[derive(Clone, Debug)]
pub struct ConstArray {
    /// Source name.
    pub name: Rc<str>,
    /// The array type.
    pub ty: Type,
    /// Explicit element values converted to the element type.
    /// Elements after this prefix are implicitly zero-initialised.
    pub init: Vec<Expr>,
}

/// A function constant.
#[derive(Clone, Debug)]
pub struct FnConst {
    /// Source name.
    pub name: Rc<str>,
    /// Scalar type.
    pub ty: Type,
    /// The `[[function_constant(i)]]` index.
    pub index: u32,
}

/// A function or entry point.
#[derive(Clone, Debug)]
pub struct Func {
    /// Source name (overloads share it).
    pub name: Rc<str>,
    /// Entry stage, None for a helper.
    pub entry: Option<Stage>,
    /// Return type.
    pub ret: Type,
    /// Parameters.
    pub params: Vec<Param>,
    /// All locals: parameters first, then declarations and temporaries.
    pub locals: Vec<Local>,
    /// The body.
    pub body: Vec<Stmt>,
    /// Attributes before the declaration (`early_fragment_tests`).
    pub attrs: Vec<Attr>,
    /// Inlining hint from the source.
    pub inline: Inline,
    /// An instantiation of an engine template (clang emits these at the end of the unit).
    pub template: bool,
    /// clang may contract `a*b + c`: the function comes before any file-scope `#pragma clang fp contract(off)`.
    pub contract: bool,
}

/// How a helper asks to be inlined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inline {
    /// No keyword.
    None,
    /// `inline`
    Hint,
    /// `__attribute__((always_inline))`
    Always,
}

/// A parameter.
#[derive(Clone, Debug)]
pub struct Param {
    /// Source name.
    pub name: Rc<str>,
    /// The value type (for a reference: the referenced type).
    pub ty: Type,
    /// `Some(space)` for a reference parameter (`thread const T&`, `constant T&`).
    pub by_ref: Option<Space>,
    /// The local holding the value (for a reference: a `Ptr(space, ty)`).
    pub local: LocalId,
    /// Attributes (`[[buffer(0)]]`, `[[stage_in]]` ...).
    pub attrs: Vec<Attr>,
    /// Declared `const` (a `const device T&` buffer is read-only).
    pub is_const: bool,
}

/// A local variable.
#[derive(Clone, Debug)]
pub struct Local {
    /// Source name, or a generated one for temporaries.
    pub name: Rc<str>,
    /// Type of the stored value.
    pub ty: Type,
    /// Static workgroup storage owned by the declaring kernel.
    pub threadgroup: bool,
}

/// A statement.
#[derive(Clone, Debug)]
pub enum Stmt {
    /// An expression evaluated for its effect.
    Expr(Expr),
    /// A typed store that may alias objects of a different type.
    MayAliasStore {
        /// The destination.
        place: Place,
        /// The value stored.
        value: Expr,
    },
    /// A nested block.
    Block(Vec<Stmt>),
    /// A nested block that starts with `#pragma clang fp contract(off)`: no `llvm.fmuladd` inside.
    NoContract(Vec<Stmt>),
    /// `if`
    If {
        /// Condition (bool).
        cond: Expr,
        /// Then branch.
        then: Vec<Stmt>,
        /// Else branch (may be empty).
        els: Vec<Stmt>,
    },
    /// `switch` with C fallthrough between arms in order.
    Switch {
        /// Integer selector.
        cond: Expr,
        /// Arms in source order.
        arms: Vec<SwitchArm>,
    },
    /// `while (cond) body`
    While {
        /// Condition.
        cond: Expr,
        /// Body.
        body: Vec<Stmt>,
    },
    /// `do body while (cond);`
    DoWhile {
        /// Body.
        body: Vec<Stmt>,
        /// Condition.
        cond: Expr,
    },
    /// `for (init; cond; step) body`
    For {
        /// Initialiser statements.
        init: Vec<Stmt>,
        /// Condition (None: true).
        cond: Option<Expr>,
        /// Step expression.
        step: Option<Expr>,
        /// Body.
        body: Vec<Stmt>,
    },
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// `return` with the value converted to the return type.
    Return(Option<Expr>),
    /// `discard_fragment()`
    Discard,
    /// A local comes into scope here (clang starts its lifetime and pushes a cleanup).
    Decl(LocalId),
}

/// One arm of a switch: its labels and the statements up to the next label.
#[derive(Clone, Debug)]
pub struct SwitchArm {
    /// Case values.`None` is `default`.
    pub labels: Vec<Option<i64>>,
    /// Statements.
    pub body: Vec<Stmt>,
}

/// A typed expression (an rvalue).
#[derive(Clone, Debug)]
pub struct Expr {
    /// What it computes.
    pub kind: ExprKind,
    /// Its type.
    pub ty: Type,
}

/// Unary operators on values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// `-x`
    Neg,
    /// `!x` on bool
    Not,
    /// `~x`
    BitNot,
}

/// Arithmetic and bitwise binary operators (operands and result share the type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `&`
    And,
    /// `|`
    Or,
    /// `^`
    Xor,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
}

/// Comparisons (operands share the type, the result is bool of the same width).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `>=`
    Ge,
}

/// A sampling option.
#[derive(Clone, Debug)]
pub enum SampleOpt {
    /// Implicit level of detail.
    None,
    /// `level(x)`
    Level(Box<Expr>),
    /// `bias(x)`
    Bias(Box<Expr>),
    /// Explicit derivatives, in x then y order.
    Grad2d(Box<Expr>, Box<Expr>),
}

/// Kind of a texture query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    /// `get_width`
    Width,
    /// `get_height`
    Height,
    /// `get_depth`
    Depth,
    /// `get_array_size`
    ArraySize,
    /// `get_num_mip_levels`
    MipLevels,
}

/// A texture operation.
#[derive(Clone, Debug)]
pub enum TexOp {
    /// `sample`, `sample_compare` or `gather`.
    Sample {
        /// `gather` with the component (0 to 3).
        gather: Option<u8>,
        /// Sampler.
        sampler: Box<Expr>,
        /// Float coordinates.
        coord: Box<Expr>,
        /// Slice of an array texture (uint).
        array: Option<Box<Expr>>,
        /// Depth reference of `sample_compare`.
        compare: Option<Box<Expr>>,
        /// Level option.
        opt: SampleOpt,
        /// Texel offset (intN), None for zero.
        offset: Option<Box<Expr>>,
    },
    /// `read(coord [, slice] [, lod])`
    Read {
        /// uint coordinates.
        coord: Box<Expr>,
        /// Slice (uint).
        array: Option<Box<Expr>>,
        /// Level (uint), None for 0.
        lod: Option<Box<Expr>>,
    },
    /// `write(value, coord [, slice] [, lod])`
    Write {
        /// The texel (vector of 4).
        value: Box<Expr>,
        /// uint coordinates.
        coord: Box<Expr>,
        /// Slice (uint).
        array: Option<Box<Expr>>,
        /// Level (uint), None for 0.
        lod: Option<Box<Expr>>,
    },
    /// `get_width(lod)` and friends.
    Query {
        /// What is asked.
        what: Query,
        /// Level (uint), None for 0.
        lod: Option<Box<Expr>>,
    },
    /// `calculate_clamped_lod` / `calculate_unclamped_lod`
    Lod {
        /// Clamped variant.
        clamped: bool,
        /// Sampler.
        sampler: Box<Expr>,
        /// Coordinates.
        coord: Box<Expr>,
    },
}

/// What an expression computes.
#[derive(Clone, Debug)]
pub enum ExprKind {
    /// An integer constant of a scalar integer or bool type.
    Int(i128),
    /// A float constant (the value as f64, for half/float it is exactly representable).
    Float(f64),
    /// The zero value of the type (`{}`, `T()`).
    Zero,
    /// Load from a place.
    Load(Box<Place>),
    /// Address of a place: a pointer value.
    AddrOf(Box<Place>),
    /// Unary operator.
    Unary(UnOp, Box<Expr>),
    /// Binary arithmetic or bitwise operator.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// Comparison.
    Compare(CmpOp, Box<Expr>, Box<Expr>),
    /// `&&` (true) or `||` (false), short-circuit on bool scalars.
    Logical(bool, Box<Expr>, Box<Expr>),
    /// `c ? a : b` (lazy).
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Numeric conversion to `ty` (same width).
    Convert(Box<Expr>),
    /// Scalar to vector broadcast (operand has the element type).
    Splat(Box<Expr>),
    /// `as_type<T>` reinterpretation (same size).
    Bitcast(Box<Expr>),
    /// Pointer reinterpretation (C cast between pointer types), or a 64-bit integer to a pointer
    /// (`reinterpret_cast`).
    PtrCast(Box<Expr>),
    /// Builds a vector from scalars and vectors of the element type.
    Vector(Vec<Expr>),
    /// Swizzle of a vector value.
    Swizzle(Box<Expr>, Vec<u8>),
    /// Assignment, the value has the place's type. Evaluates to the stored value.
    Assign(Box<Place>, Box<Expr>),
    /// `place op= value`: computed in `op_ty`, stored back converted.
    CompoundAssign {
        /// Operator.
        op: BinOp,
        /// Target.
        place: Box<Place>,
        /// Right operand, already in `op_ty`.
        value: Box<Expr>,
        /// Type the operation is done in.
        op_ty: Type,
    },
    /// Call of a user function.
    Call(FuncId, Vec<Arg>),
    /// Call of a metal_stdlib function, arguments already have the overload's types.
    Builtin(&'static str, Vec<Expr>),
    /// Texture operation on a texture value.
    Texture {
        /// The texture handle.
        tex: Box<Expr>,
        /// Its type.
        tex_ty: Tex,
        /// The operation.
        op: TexOp,
    },
    /// The value of a function constant.
    FnConst(FcId),
    /// Pointer plus integer offset in elements.
    PtrOffset(Box<Expr>, Box<Expr>),
    /// Struct value from field values (missing trailing fields are zero).
    StructInit(Vec<Expr>),
    /// Statements evaluated before a value (desugared engine helpers).
    Seq(Vec<Stmt>, Box<Expr>),
    /// Reinterpret a texture handle as another texture kind (`spvDepthCast`).
    TexCast(Box<Expr>),
}

/// An argument of a user function call.
#[derive(Clone, Debug)]
pub enum Arg {
    /// By value, converted to the parameter type.
    Value(Expr),
    /// By reference to a place.
    Ref(Place),
}

/// Something with an address: the target of loads, stores and references.
#[derive(Clone, Debug)]
pub struct Place {
    /// How the address is formed.
    pub kind: PlaceKind,
    /// Type of the stored value.
    pub ty: Type,
    /// Address space.
    pub space: Space,
}

/// How a place's address is formed.
#[derive(Clone, Debug)]
pub enum PlaceKind {
    /// A local variable (thread space).
    Local(LocalId),
    /// A global constant array.
    Const(ConstId),
    /// What a pointer value points to.
    Deref(Box<Expr>),
    /// A struct field by index.
    Field(Box<Place>, u32),
    /// An array element.
    Index(Box<Place>, Box<Expr>),
    /// A vector component by a dynamic or constant index.
    Element(Box<Place>, Box<Expr>),
    /// A swizzle of a vector place (distinct components when stored to).
    Swizzle(Box<Place>, Vec<u8>),
    /// A temporary holding the value of an rvalue.
    Temp(Box<Expr>),
}

impl Expr {
    /// A new expression.
    pub fn new(kind: ExprKind, ty: Type) -> Expr {
        Expr { kind, ty }
    }

    /// An integer constant.
    pub fn int(v: i128, ty: Type) -> Expr {
        Expr {
            kind: ExprKind::Int(v),
            ty,
        }
    }
}

/// Calls `f` with every user function a statement list calls (directly).
pub fn visit_calls(stmts: &[Stmt], f: &mut dyn FnMut(FuncId)) {
    for s in stmts {
        visit_stmt(s, f);
    }
}

fn visit_stmt(s: &Stmt, f: &mut dyn FnMut(FuncId)) {
    match s {
        Stmt::Expr(e) => visit_expr(e, f),
        Stmt::MayAliasStore { place, value } => {
            visit_place(place, f);
            visit_expr(value, f);
        }
        Stmt::Block(b) | Stmt::NoContract(b) => visit_calls(b, f),
        Stmt::If { cond, then, els } => {
            visit_expr(cond, f);
            visit_calls(then, f);
            visit_calls(els, f);
        }
        Stmt::Switch { cond, arms } => {
            visit_expr(cond, f);
            for a in arms {
                visit_calls(&a.body, f);
            }
        }
        Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
            visit_expr(cond, f);
            visit_calls(body, f);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => {
            visit_calls(init, f);
            if let Some(c) = cond {
                visit_expr(c, f);
            }
            if let Some(c) = step {
                visit_expr(c, f);
            }
            visit_calls(body, f);
        }
        Stmt::Return(Some(e)) => visit_expr(e, f),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Discard | Stmt::Decl(_) => {}
    }
}

fn visit_place(p: &Place, f: &mut dyn FnMut(FuncId)) {
    match &p.kind {
        PlaceKind::Local(_) | PlaceKind::Const(_) => {}
        PlaceKind::Deref(e) | PlaceKind::Temp(e) => visit_expr(e, f),
        PlaceKind::Field(b, _) | PlaceKind::Swizzle(b, _) => visit_place(b, f),
        PlaceKind::Index(b, i) | PlaceKind::Element(b, i) => {
            visit_place(b, f);
            visit_expr(i, f);
        }
    }
}

fn visit_expr(e: &Expr, f: &mut dyn FnMut(FuncId)) {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Zero | ExprKind::FnConst(_) => {}
        ExprKind::Load(p) | ExprKind::AddrOf(p) => visit_place(p, f),
        ExprKind::Unary(_, x)
        | ExprKind::Convert(x)
        | ExprKind::Splat(x)
        | ExprKind::Bitcast(x)
        | ExprKind::PtrCast(x)
        | ExprKind::Swizzle(x, _)
        | ExprKind::TexCast(x) => visit_expr(x, f),
        ExprKind::Binary(_, a, b)
        | ExprKind::Compare(_, a, b)
        | ExprKind::Logical(_, a, b)
        | ExprKind::PtrOffset(a, b) => {
            visit_expr(a, f);
            visit_expr(b, f);
        }
        ExprKind::Cond(a, b, c) => {
            visit_expr(a, f);
            visit_expr(b, f);
            visit_expr(c, f);
        }
        ExprKind::Vector(v) | ExprKind::Builtin(_, v) | ExprKind::StructInit(v) => {
            for x in v {
                visit_expr(x, f);
            }
        }
        ExprKind::Assign(p, x)
        | ExprKind::CompoundAssign {
            place: p, value: x, ..
        } => {
            visit_place(p, f);
            visit_expr(x, f);
        }
        ExprKind::Call(id, args) => {
            f(*id);
            for a in args {
                match a {
                    Arg::Value(x) => visit_expr(x, f),
                    Arg::Ref(p) => visit_place(p, f),
                }
            }
        }
        ExprKind::Texture { tex, op, .. } => {
            visit_expr(tex, f);
            let opt = |x: &Option<Box<Expr>>, f: &mut dyn FnMut(FuncId)| {
                if let Some(x) = x {
                    visit_expr(x, f);
                }
            };
            match op {
                TexOp::Sample {
                    sampler,
                    coord,
                    array,
                    compare,
                    opt: so,
                    offset,
                    ..
                } => {
                    visit_expr(sampler, f);
                    visit_expr(coord, f);
                    opt(array, f);
                    opt(compare, f);
                    opt(offset, f);
                    match so {
                        SampleOpt::None => {}
                        SampleOpt::Level(x) | SampleOpt::Bias(x) => visit_expr(x, f),
                        SampleOpt::Grad2d(dx, dy) => {
                            visit_expr(dx, f);
                            visit_expr(dy, f);
                        }
                    }
                }
                TexOp::Read { coord, array, lod } => {
                    visit_expr(coord, f);
                    opt(array, f);
                    opt(lod, f);
                }
                TexOp::Write {
                    value,
                    coord,
                    array,
                    lod,
                } => {
                    visit_expr(value, f);
                    visit_expr(coord, f);
                    opt(array, f);
                    opt(lod, f);
                }
                TexOp::Query { lod, .. } => opt(lod, f),
                TexOp::Lod { sampler, coord, .. } => {
                    visit_expr(sampler, f);
                    visit_expr(coord, f);
                }
            }
        }
        ExprKind::Seq(s, x) => {
            visit_calls(s, f);
            visit_expr(x, f);
        }
    }
}
