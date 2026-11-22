//! The text IR subset air-emit writes, as a tree.

/// A type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    /// `void`
    Void,
    /// `iN`
    Int(u32),
    /// `half`
    Half,
    /// `float`
    Float,
    /// `double`
    Double,
    /// `label`
    Label,
    /// `metadata`
    Metadata,
    /// `T addrspace(n)*`
    Ptr(Box<Ty>, u32),
    /// `<n x T>`
    Vector(u32, Box<Ty>),
    /// `[n x T]`
    Array(u64, Box<Ty>),
    /// `{ ... }` or `<{ ... }>`
    Struct(bool, Vec<Ty>),
    /// `%name`
    Named(String),
    /// `R (P...)`
    Func(Box<Ty>, Vec<Ty>),
}

/// A value operand.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Val {
    /// `%x`
    Local(String),
    /// `@x`
    Global(String),
    /// integer (also `true`/`false`)
    Int(i128),
    /// float bits as written (`0x3FF0...` double form or `0xH3C00`)
    Float(u64),
    /// `null`
    Null,
    /// `undef`
    Undef,
    /// `poison`
    Poison,
    /// `zeroinitializer`
    Zero,
    /// `<T a, ...>`, `[T a, ...]`, `{ T a, ...}`
    Aggregate(Vec<(Ty, Val)>),
    /// `bitcast (T v to U)`, `addrspacecast (...)`
    Cast(String, Box<(Ty, Val)>, Ty),
}

/// A metadata operand.
#[derive(Clone, Debug, PartialEq)]
pub enum MdOp {
    /// `!"text"`
    Str(Vec<u8>),
    /// `!N`
    Node(u32),
    /// `T v` as metadata
    Value(Ty, Val),
}

/// An instruction.
#[derive(Clone, Debug)]
pub struct Inst {
    /// Result name.
    pub res: Option<String>,
    /// The operation.
    pub op: Op,
    /// `!kind !N` attachments.
    pub md: Vec<(String, u32)>,
}

/// Operations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// alloca T, align A
    Alloca(Ty, u32),
    /// load [volatile] T, ptr, align A; the final field is volatility.
    Load(Ty, (Ty, Val), u32, bool),
    /// store (T v), (ptr), align A
    Store((Ty, Val), (Ty, Val), u32),
    /// getelementptr [inbounds] T, ptr, idx...
    Gep(bool, Ty, (Ty, Val), Vec<(Ty, Val)>),
    /// cast op, value, to type
    Cast(String, (Ty, Val), Ty),
    /// binary op with flags (nsw/fast-math words)
    Bin(String, Vec<String>, Ty, Val, Val),
    /// fneg with flags
    FNeg(Vec<String>, Ty, Val),
    /// freeze T v
    Freeze(Ty, Val),
    /// icmp/fcmp: is_float, flags, predicate, type, a, b
    Cmp(bool, Vec<String>, String, Ty, Val, Val),
    /// select flags, cond, a, b
    Select(Vec<String>, (Ty, Val), (Ty, Val), (Ty, Val)),
    /// phi flags, type, (value, block)
    Phi(Vec<String>, Ty, Vec<(Val, String)>),
    /// br label
    Br(String),
    /// br i1 c, label a, label b
    CondBr(Val, String, String),
    /// switch T v, label d [ (k, label) ]
    Switch(Ty, Val, String, Vec<(Val, String)>),
    /// ret
    Ret(Option<(Ty, Val)>),
    /// unreachable
    Unreachable,
    /// call flags, return attrs, ret type, callee, args (type, attrs, value), call-site attribute group
    Call(
        Vec<String>,
        Vec<String>,
        Ty,
        String,
        Vec<(Ty, Vec<String>, Val)>,
        Option<u32>,
    ),
    /// extractelement
    ExtractElt((Ty, Val), (Ty, Val)),
    /// insertelement
    InsertElt((Ty, Val), (Ty, Val), (Ty, Val)),
    /// shufflevector a, b, mask
    Shuffle((Ty, Val), (Ty, Val), (Ty, Val)),
    /// extractvalue v, idx
    ExtractVal((Ty, Val), Vec<u32>),
    /// insertvalue v, e, idx
    InsertVal((Ty, Val), (Ty, Val), Vec<u32>),
}

/// A parameter: type, attributes, name.
#[derive(Clone, Debug)]
pub struct ParamDef {
    /// Type.
    pub ty: Ty,
    /// Attributes (`noundef`, `align 8`, `"air-buffer-no-alias"` ...).
    pub attrs: Vec<String>,
    /// Name (definitions).
    pub name: Option<String>,
}

/// A function declaration or definition.
#[derive(Clone, Debug)]
pub struct Function {
    /// Name without `@`.
    pub name: String,
    /// Linkage word (`internal`, `linkonce_odr` ...) or empty.
    pub linkage: String,
    /// Return attributes (`zeroext`).
    pub ret_attrs: Vec<String>,
    /// Return type.
    pub ret: Ty,
    /// Parameters.
    pub params: Vec<ParamDef>,
    /// Attribute group.
    pub group: Option<u32>,
    /// `section "..."`
    pub section: Option<String>,
    /// Blocks: (label, instructions), empty for a declaration.
    pub blocks: Vec<(String, Vec<Inst>)>,
}

/// A global variable.
#[derive(Clone, Debug)]
pub struct Global {
    /// Name without `@`.
    pub name: String,
    /// Linkage word.
    pub linkage: String,
    /// `hidden`
    pub hidden: bool,
    /// Address space.
    pub space: u32,
    /// `externally_initialized`
    pub ext_init: bool,
    /// `constant` (else `global`)
    pub constant: bool,
    /// Value type.
    pub ty: Ty,
    /// Initialiser.
    pub init: Val,
    /// `section`
    pub section: Option<String>,
    /// `align`
    pub align: u32,
}

/// A named struct: name and `Some((packed, elements))`, or `None` when opaque.
pub type NamedType = (String, Option<(bool, Vec<Ty>)>);

/// A module.
#[derive(Clone, Debug, Default)]
pub struct Module {
    /// `source_filename`
    pub source: String,
    /// datalayout
    pub layout: String,
    /// triple
    pub triple: String,
    /// named struct types in order: (name, Some(packed, elems) or None for opaque)
    pub types: Vec<NamedType>,
    /// globals in order
    pub globals: Vec<Global>,
    /// functions in order
    pub functions: Vec<Function>,
    /// attribute groups: number -> words
    pub attr_groups: Vec<(u32, Vec<String>)>,
    /// named metadata: (name, nodes)
    pub named_md: Vec<(String, Vec<u32>)>,
    /// metadata nodes: number -> (distinct, operands)
    pub md: Vec<(u32, bool, Vec<MdOp>)>,
}
