//! Types of the checker, modelled on the prototype's `Ty` class.
//!
//! The prototype's `Ty(k, a, b, c, d)` is dynamically typed. This module gives every kind its
//! own variant, with one deliberate concession to exact compatibility: a scalar's name is a
//! [`ScalarName`], which can be *bogus*. The prototype's builtins `dot`, `length` and
//! `distance` return `S(a[0].a)`, the "scalar" named after whatever field `a` the first
//! argument has (a struct name, an address space, an element type ...). Such a type behaves
//! like a scalar until something looks its name up in the scalar table, where the prototype
//! raises `KeyError`. Keeping that behaviour is what makes the verdicts identical on odd input.
//! For well-formed engine output the name is always [`ScalarName::Known`].

use std::fmt;
use std::rc::Rc;

use crate::Fail;

/// The scalar types of the prototype's `SCALARS` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scalar {
    Bool,
    Char,
    Uchar,
    Short,
    Ushort,
    Int,
    Uint,
    Long,
    Ulong,
    Half,
    Float,
    SizeT,
}

/// Numeric class of a scalar: `b`ool, signed `i`nteger, `u`nsigned integer, `f`loat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    B,
    I,
    U,
    F,
}

impl Scalar {
    pub const ALL: [Scalar; 12] = [
        Scalar::Bool,
        Scalar::Char,
        Scalar::Uchar,
        Scalar::Short,
        Scalar::Ushort,
        Scalar::Int,
        Scalar::Uint,
        Scalar::Long,
        Scalar::Ulong,
        Scalar::Half,
        Scalar::Float,
        Scalar::SizeT,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scalar::Bool => "bool",
            Scalar::Char => "char",
            Scalar::Uchar => "uchar",
            Scalar::Short => "short",
            Scalar::Ushort => "ushort",
            Scalar::Int => "int",
            Scalar::Uint => "uint",
            Scalar::Long => "long",
            Scalar::Ulong => "ulong",
            Scalar::Half => "half",
            Scalar::Float => "float",
            Scalar::SizeT => "size_t",
        }
    }

    pub fn from_name(s: &str) -> Option<Scalar> {
        Some(match s {
            "bool" => Scalar::Bool,
            "char" => Scalar::Char,
            "uchar" => Scalar::Uchar,
            "short" => Scalar::Short,
            "ushort" => Scalar::Ushort,
            "int" => Scalar::Int,
            "uint" => Scalar::Uint,
            "long" => Scalar::Long,
            "ulong" => Scalar::Ulong,
            "half" => Scalar::Half,
            "float" => Scalar::Float,
            "size_t" => Scalar::SizeT,
            _ => return None,
        })
    }

    /// `SCALARS[name][0]`
    pub fn class(self) -> Class {
        match self {
            Scalar::Bool => Class::B,
            Scalar::Char | Scalar::Short | Scalar::Int | Scalar::Long => Class::I,
            Scalar::Uchar | Scalar::Ushort | Scalar::Uint | Scalar::Ulong | Scalar::SizeT => {
                Class::U
            }
            Scalar::Half | Scalar::Float => Class::F,
        }
    }

    /// `SCALARS[name][1]`: size in bytes.
    pub fn size(self) -> u32 {
        match self {
            Scalar::Bool | Scalar::Char | Scalar::Uchar => 1,
            Scalar::Short | Scalar::Ushort | Scalar::Half => 2,
            Scalar::Int | Scalar::Uint | Scalar::Float => 4,
            Scalar::Long | Scalar::Ulong | Scalar::SizeT => 8,
        }
    }

    /// `RANK.get(name)`: integer conversion rank. Floats have none.
    pub fn rank(self) -> Option<u32> {
        match self {
            Scalar::Bool => Some(0),
            Scalar::Char | Scalar::Uchar => Some(1),
            Scalar::Short | Scalar::Ushort => Some(2),
            Scalar::Int | Scalar::Uint => Some(3),
            Scalar::Long | Scalar::Ulong | Scalar::SizeT => Some(4),
            Scalar::Half | Scalar::Float => None,
        }
    }
}

/// The name of a scalar type. See the module documentation for the bogus case.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ScalarName {
    Known(Scalar),
    /// A non-scalar name string (struct name, address space, texture kind ...).
    BogusStr(Rc<str>),
    /// The element type of an array.
    BogusTy(Rc<Ty>),
    /// Python's `None` (the `a` field of `sampler` and `void`).
    BogusNone,
}

impl ScalarName {
    /// `S(name)` for a name string: a known scalar when the name is one.
    pub fn named(s: &str) -> ScalarName {
        match Scalar::from_name(s) {
            Some(k) => ScalarName::Known(k),
            None => ScalarName::BogusStr(s.into()),
        }
    }

    /// `SCALARS[name]`: `KeyError` for a bogus name.
    pub fn known(&self) -> Result<Scalar, Fail> {
        match self {
            ScalarName::Known(k) => Ok(*k),
            _ => Err(Fail::Crash(PyExc::KeyError)),
        }
    }

    pub fn class(&self) -> Result<Class, Fail> {
        Ok(self.known()?.class())
    }

    /// `RANK.get(name, default)`
    pub fn rank_or(&self, default: u32) -> u32 {
        match self {
            ScalarName::Known(k) => k.rank().unwrap_or(default),
            _ => default,
        }
    }

    pub fn is(&self, s: Scalar) -> bool {
        matches!(self, ScalarName::Known(k) if *k == s)
    }

    /// `str(name)` as an f-string inserts it: `TypeError` when the prototype's `__repr__`
    /// would return a non-string.
    fn display(&self) -> Result<String, Fail> {
        match self {
            ScalarName::Known(k) => Ok(k.name().to_string()),
            ScalarName::BogusStr(s) => Ok(s.to_string()),
            ScalarName::BogusTy(t) => t.repr(),
            ScalarName::BogusNone => Ok("None".to_string()),
        }
    }
}

/// Python exceptions the prototype can raise besides its own `Reject`. A crash aborts the
/// whole file: `parse_file` turns `RecursionError` into the reason `recursion`, the others
/// would end the prototype's run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PyExc {
    IndexError,
    KeyError,
    TypeError,
    ValueError,
    RecursionError,
}

impl PyExc {
    pub fn name(self) -> &'static str {
        match self {
            PyExc::IndexError => "IndexError",
            PyExc::KeyError => "KeyError",
            PyExc::TypeError => "TypeError",
            PyExc::ValueError => "ValueError",
            PyExc::RecursionError => "RecursionError",
        }
    }
}

/// Address spaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Space {
    Device,
    Constant,
    Threadgroup,
    Thread,
}

impl Space {
    pub fn name(self) -> &'static str {
        match self {
            Space::Device => "device",
            Space::Constant => "constant",
            Space::Threadgroup => "threadgroup",
            Space::Thread => "thread",
        }
    }

    pub fn from_name(s: &str) -> Option<Space> {
        Some(match s {
            "device" => Space::Device,
            "constant" => Space::Constant,
            "threadgroup" => Space::Threadgroup,
            "thread" => Space::Thread,
            _ => return None,
        })
    }
}

/// Texture kinds of the prototype's `TEX_KINDS` with their coordinate dimension.
pub fn tex_dim(kind: &str) -> Option<u8> {
    Some(match kind {
        "texture2d" | "texture2d_array" | "depth2d" | "depth2d_array" | "texture2d_ms"
        | "texture2d_ms_array" => 2,
        "texture3d" | "texturecube" | "texturecube_array" | "depthcube" | "depthcube_array" => 3,
        "texture1d" | "texture_buffer" => 1,
        _ => return None,
    })
}

/// A type. Equality is the prototype's `Ty.key()` equality.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Scalar(ScalarName),
    /// `w` is 2 to 4. `packed` is the prototype's `c == 'packed'` (part of equality).
    Vec {
        base: ScalarName,
        w: u8,
        packed: bool,
    },
    Mat {
        base: Scalar,
        cols: u8,
        rows: u8,
    },
    Struct(Rc<str>),
    Ptr(Space, Rc<Ty>),
    /// Opaque word pointer usable by relaxed threadgroup atomics.
    ThreadgroupAtomicUintPtr,
    /// Opaque pointer usable by relaxed device unsigned atomic OR.
    DeviceAtomicUintPtr,
    /// An lvalue (or reference) of the inner type in an address space.
    Ref(Space, Rc<Ty>),
    /// `n` is the decimal length without leading zeros. `unsafe_` is `spvUnsafeArray`, `metal`
    /// is `metal::array<T, n>`.
    Array {
        elem: Rc<Ty>,
        n: Rc<str>,
        unsafe_: bool,
        metal: bool,
    },
    /// `kind` may be a name outside `TEX_KINDS` (see `spvDepthCast`).
    Tex {
        kind: Rc<str>,
        el: Scalar,
        access: Rc<str>,
    },
    Sampler,
    Void,
    /// A sampling option such as `level(x)`.
    Opt(&'static str),
    /// `component::x`
    Comp(Rc<str>),
}

pub const BOOL: Ty = Ty::Scalar(ScalarName::Known(Scalar::Bool));
pub const INT: Ty = Ty::Scalar(ScalarName::Known(Scalar::Int));
pub const UINT: Ty = Ty::Scalar(ScalarName::Known(Scalar::Uint));
pub const FLOAT: Ty = Ty::Scalar(ScalarName::Known(Scalar::Float));
pub const HALF: Ty = Ty::Scalar(ScalarName::Known(Scalar::Half));

/// `S(name)`
pub fn s(k: Scalar) -> Ty {
    Ty::Scalar(ScalarName::Known(k))
}

/// `V(name, w)`: a scalar for width 1.
pub fn v(base: ScalarName, w: u8) -> Ty {
    if w == 1 {
        Ty::Scalar(base)
    } else {
        Ty::Vec {
            base,
            w,
            packed: false,
        }
    }
}

pub fn vk(base: Scalar, w: u8) -> Ty {
    v(ScalarName::Known(base), w)
}

impl Ty {
    /// The prototype's kind string `k`.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Ty::Scalar(_) => "scalar",
            Ty::Vec { .. } => "vec",
            Ty::Mat { .. } => "mat",
            Ty::Struct(_) => "struct",
            Ty::Ptr(..) => "ptr",
            Ty::ThreadgroupAtomicUintPtr => "threadgroup atomic_uint*",
            Ty::DeviceAtomicUintPtr => "device atomic_uint*",
            Ty::Ref(..) => "ref",
            Ty::Array { .. } => "array",
            Ty::Tex { .. } => "tex",
            Ty::Sampler => "sampler",
            Ty::Void => "void",
            Ty::Opt(_) => "opt",
            Ty::Comp(_) => "comp",
        }
    }

    /// `strip(t)`: the value type of a reference.
    pub fn strip(&self) -> Ty {
        match self {
            Ty::Ref(_, inner) => (**inner).clone(),
            t => t.clone(),
        }
    }

    pub fn is_ref(&self) -> bool {
        matches!(self, Ty::Ref(..))
    }

    /// `isnum(t)`
    pub fn is_num(&self) -> bool {
        matches!(self, Ty::Scalar(_) | Ty::Vec { .. })
    }

    /// `t.a` of a numeric type.
    pub fn num_base(&self) -> Option<&ScalarName> {
        match self {
            Ty::Scalar(n) => Some(n),
            Ty::Vec { base, .. } => Some(base),
            _ => None,
        }
    }

    /// `width(t)`: None for non-numeric types.
    pub fn width(&self) -> Option<u8> {
        match self {
            Ty::Scalar(_) => Some(1),
            Ty::Vec { w, .. } => Some(*w),
            _ => None,
        }
    }

    /// `isfloat(t)` (`KeyError` for a bogus name).
    pub fn is_float(&self) -> Result<bool, Fail> {
        match self.num_base() {
            Some(n) => Ok(n.class()? == Class::F),
            None => Ok(false),
        }
    }

    /// `isint(t)` (`KeyError` for a bogus name).
    pub fn is_int(&self) -> Result<bool, Fail> {
        match self.num_base() {
            Some(n) => Ok(matches!(n.class()?, Class::I | Class::U)),
            None => Ok(false),
        }
    }

    /// `isbool(t)`
    pub fn is_bool(&self) -> bool {
        self.num_base().is_some_and(|n| n.is(Scalar::Bool))
    }

    /// `S(t.a)`: the scalar the builtins `dot`, `length` and `distance` return.
    pub fn a_as_scalar(&self) -> Ty {
        Ty::Scalar(match self {
            Ty::Scalar(n) => n.clone(),
            Ty::Vec { base, .. } => base.clone(),
            Ty::Mat { base, .. } => ScalarName::Known(*base),
            Ty::Struct(name) => ScalarName::named(name),
            Ty::Ptr(space, _) | Ty::Ref(space, _) => ScalarName::named(space.name()),
            Ty::Array { elem, .. } => ScalarName::BogusTy(elem.clone()),
            Ty::Tex { kind, .. } => ScalarName::named(kind),
            Ty::ThreadgroupAtomicUintPtr | Ty::DeviceAtomicUintPtr | Ty::Sampler | Ty::Void => {
                ScalarName::BogusNone
            }
            Ty::Opt(name) => ScalarName::named(name),
            Ty::Comp(name) => ScalarName::named(name),
        })
    }

    /// The prototype's `repr(t)`. Fails with `TypeError` where its `__repr__` would return a
    /// non-string (a bogus scalar whose name is not a string).
    pub fn repr(&self) -> Result<String, Fail> {
        Ok(match self {
            Ty::Scalar(n) => match n {
                ScalarName::Known(k) => k.name().to_string(),
                ScalarName::BogusStr(s) => s.to_string(),
                ScalarName::BogusTy(_) | ScalarName::BogusNone => {
                    return Err(Fail::Crash(PyExc::TypeError))
                }
            },
            Ty::Vec { base, w, .. } => format!("{}{}", base.display()?, w),
            Ty::Mat { base, cols, rows } => format!("{}{}x{}", base.name(), cols, rows),
            Ty::Struct(name) => name.to_string(),
            Ty::Ptr(space, inner) => format!("{} {}*", space.name(), inner.repr()?),
            Ty::Ref(space, inner) => format!("{} {}&", space.name(), inner.repr()?),
            Ty::Array { elem, n, .. } => format!("{}[{}]", elem.repr()?, n),
            Ty::Tex { kind, el, access } => format!("{}<{},{}>", kind, el.name(), access),
            other => other.kind_name().to_string(),
        })
    }
}

/// `repr()` of a Python list of types.
pub fn repr_list(ts: &[Ty]) -> Result<String, Fail> {
    let mut parts = Vec::with_capacity(ts.len());
    for t in ts {
        parts.push(t.repr()?);
    }
    Ok(format!("[{}]", parts.join(", ")))
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.repr() {
            Ok(s) => f.write_str(&s),
            Err(_) => write!(f, "<{}>", self.kind_name()),
        }
    }
}

/// The usual arithmetic conversions for two scalar names (`arith(a, b)`).
pub fn arith(a: &ScalarName, b: &ScalarName) -> Result<ScalarName, Fail> {
    let ka = a.known()?;
    let kb = b.known()?;
    let (fa, fb) = (ka.class() == Class::F, kb.class() == Class::F);
    if fa || fb {
        if fa && fb {
            return Ok(ScalarName::Known(
                if ka == Scalar::Float || kb == Scalar::Float {
                    Scalar::Float
                } else {
                    Scalar::Half
                },
            ));
        }
        return Ok(ScalarName::Known(if fa { ka } else { kb }));
    }
    // Both are integers or bool here, so both have a rank.
    let (rka, rkb) = (ka.rank().unwrap_or(9), kb.rank().unwrap_or(9));
    let (ra, rb) = (rka.max(3), rkb.max(3));
    let pa = if rka >= 3 { ka } else { Scalar::Int };
    let pb = if rkb >= 3 { kb } else { Scalar::Int };
    // The branches follow the prototype's `arith` line by line, including the two that agree.
    #[allow(clippy::if_same_then_else)]
    let r = if ra != rb {
        if ra > rb {
            pa
        } else {
            pb
        }
    } else if pa == pb {
        pa
    } else if pa.class() == Class::U {
        pa
    } else {
        pb
    };
    Ok(ScalarName::Known(r))
}

/// Parses a vector or scalar type name: `(packed_)?(bool|char|...|float)([234])?`.
/// Returns (packed, base, width digit if present).
pub fn vec_name(name: &str) -> Option<(bool, Scalar, Option<u8>)> {
    let (packed, rest) = match name.strip_prefix("packed_") {
        Some(r) => (true, r),
        None => (false, name),
    };
    let (base, w) = match rest.as_bytes().last() {
        Some(&d @ b'2'..=b'4') => (&rest[..rest.len() - 1], Some(d - b'0')),
        _ => (rest, None),
    };
    let k = Scalar::from_name(base)?;
    if k == Scalar::SizeT {
        return None;
    }
    Some((packed, k, w))
}

/// Parses a matrix type name: `(half|float)([234])x([234])`.
pub fn mat_name(name: &str) -> Option<(Scalar, u8, u8)> {
    let (base, rest) = if let Some(r) = name.strip_prefix("float") {
        (Scalar::Float, r)
    } else {
        let r = name.strip_prefix("half")?;
        (Scalar::Half, r)
    };
    match rest.as_bytes() {
        [c @ b'2'..=b'4', b'x', r @ b'2'..=b'4'] => Some((base, c - b'0', r - b'0')),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: Scalar) -> ScalarName {
        ScalarName::Known(s)
    }

    #[test]
    fn arithmetic_conversions() {
        use Scalar::*;
        assert_eq!(arith(&k(Int), &k(Uint)).unwrap(), k(Uint));
        assert_eq!(arith(&k(Short), &k(Ushort)).unwrap(), k(Int));
        assert_eq!(arith(&k(Uchar), &k(Uint)).unwrap(), k(Uint));
        assert_eq!(arith(&k(Long), &k(Uint)).unwrap(), k(Long));
        assert_eq!(arith(&k(Half), &k(Int)).unwrap(), k(Half));
        assert_eq!(arith(&k(Half), &k(Float)).unwrap(), k(Float));
        assert_eq!(arith(&k(Bool), &k(Bool)).unwrap(), k(Int));
        assert_eq!(arith(&k(Long), &k(Ulong)).unwrap(), k(Ulong));
        assert_eq!(arith(&k(SizeT), &k(Long)).unwrap(), k(SizeT));
        assert!(arith(&ScalarName::BogusStr("S".into()), &k(Int)).is_err());
    }

    #[test]
    fn names() {
        assert_eq!(
            vec_name("packed_float3"),
            Some((true, Scalar::Float, Some(3)))
        );
        assert_eq!(vec_name("uint"), Some((false, Scalar::Uint, None)));
        assert_eq!(vec_name("packed_half"), Some((true, Scalar::Half, None)));
        assert_eq!(vec_name("size_t2"), None);
        assert_eq!(vec_name("float5"), None);
        assert_eq!(vec_name("float1"), None);
        assert_eq!(mat_name("float4x3"), Some((Scalar::Float, 4, 3)));
        assert_eq!(mat_name("int4x4"), None);
    }

    #[test]
    fn repr_matches_prototype() {
        assert_eq!(vk(Scalar::Float, 4).repr().unwrap(), "float4");
        let p = Ty::Ptr(Space::Device, Rc::new(Ty::Struct("S".into())));
        assert_eq!(p.repr().unwrap(), "device S*");
        let a = Ty::Array {
            elem: Rc::new(UINT),
            n: "4".into(),
            unsafe_: true,
            metal: false,
        };
        assert_eq!(a.repr().unwrap(), "uint[4]");
        let t = Ty::Tex {
            kind: "texture2d".into(),
            el: Scalar::Float,
            access: "sample".into(),
        };
        assert_eq!(t.repr().unwrap(), "texture2d<float,sample>");
        assert_eq!(Ty::Sampler.repr().unwrap(), "sampler");
        assert!(Ty::Scalar(ScalarName::BogusNone).repr().is_err());
        assert_eq!(repr_list(&[INT, FLOAT]).unwrap(), "[int, float]");
    }

    #[test]
    fn equality_includes_packing_and_unsafe() {
        let a = Ty::Vec {
            base: k(Scalar::Uint),
            w: 3,
            packed: true,
        };
        assert_ne!(a, vk(Scalar::Uint, 3));
        assert_eq!(ScalarName::named("int"), k(Scalar::Int));
    }
}
