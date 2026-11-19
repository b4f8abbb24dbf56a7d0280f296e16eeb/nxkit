//! Overloads of the metal_stdlib functions the engine uses: which argument types a call binds
//! to. The emitter maps the resolved name and types to an AIR intrinsic.

use crate::hir::Expr;
use crate::types::{Scalar, Type};
use crate::{conv, Error};

/// How a builtin's arguments bind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Every argument takes the result type (scalars are broadcast).
    Same,
    /// The argument keeps its type (`isnan`, `all` ...).
    Keep,
    /// `select(a, b, cond)`: a and b take the result type, cond keeps its type.
    Select,
    /// `extract_bits(x, uint, uint)`
    Extract,
    /// `insert_bits(x, y, uint, uint)`
    Insert,
    /// One uint argument (`unpack_*`).
    Uint,
    /// `simd_shuffle(x, ushort lane)`: x takes the result type, the lane becomes a ushort.
    Lane,
}

/// Builtins with their binding shape. The names are the ones the parser reports.
const TABLE: &[(&str, Shape)] = &[
    ("abs", Shape::Same),
    ("sqrt", Shape::Same),
    ("rsqrt", Shape::Same),
    ("exp2", Shape::Same),
    ("log2", Shape::Same),
    ("floor", Shape::Same),
    ("ceil", Shape::Same),
    ("trunc", Shape::Same),
    ("rint", Shape::Same),
    ("round", Shape::Same),
    ("sin", Shape::Same),
    ("cos", Shape::Same),
    ("tan", Shape::Same),
    ("fract", Shape::Same),
    ("saturate", Shape::Same),
    ("sign", Shape::Same),
    ("dfdx", Shape::Same),
    ("dfdy", Shape::Same),
    ("fwidth", Shape::Same),
    ("exp", Shape::Same),
    ("log", Shape::Same),
    ("min", Shape::Same),
    ("max", Shape::Same),
    ("precise::min", Shape::Same),
    ("precise::max", Shape::Same),
    ("fast::min", Shape::Same),
    ("fast::max", Shape::Same),
    ("fmin", Shape::Same),
    ("fmax", Shape::Same),
    ("pow", Shape::Same),
    ("powr", Shape::Same),
    ("fma", Shape::Same),
    ("clamp", Shape::Same),
    ("precise::clamp", Shape::Same),
    ("fast::clamp", Shape::Same),
    ("mix", Shape::Same),
    ("popcount", Shape::Same),
    ("clz", Shape::Same),
    ("ctz", Shape::Same),
    ("reverse_bits", Shape::Same),
    ("mulhi", Shape::Same),
    ("simd_shuffle", Shape::Lane),
    ("threadgroup_barrier", Shape::Keep),
    ("simd_any", Shape::Keep),
    ("simd_all", Shape::Keep),
    ("isnan", Shape::Keep),
    ("isinf", Shape::Keep),
    ("isfinite", Shape::Keep),
    ("all", Shape::Keep),
    ("any", Shape::Keep),
    ("select", Shape::Select),
    ("extract_bits", Shape::Extract),
    ("insert_bits", Shape::Insert),
    ("unpack_unorm10a2_to_float", Shape::Uint),
    ("unpack_unorm4x8_to_float", Shape::Uint),
    ("unpack_snorm4x8_to_float", Shape::Uint),
    ("unpack_unorm2x16_to_float", Shape::Uint),
    ("unpack_snorm2x16_to_float", Shape::Uint),
];

/// The static name of a supported builtin.
pub fn known(name: &str) -> Option<&'static str> {
    TABLE.iter().find(|(n, _)| *n == name).map(|(n, _)| *n)
}

/// Binds the arguments of builtin `name` whose call has type `ret`.
pub fn bind(name: &str, args: Vec<Expr>, ret: &Type) -> Result<(&'static str, Vec<Expr>), Error> {
    let Some(&(sname, shape)) = TABLE.iter().find(|(n, _)| *n == name) else {
        return Err(Error::unsupported(format!("builtin:{name}")));
    };
    let uint = Type::Scalar(Scalar::Uint);
    let out = match shape {
        Shape::Same => args
            .into_iter()
            .map(|a| conv(a, ret))
            .collect::<Result<Vec<_>, _>>()?,
        Shape::Keep => args,
        Shape::Uint => args
            .into_iter()
            .map(|a| conv(a, &uint))
            .collect::<Result<Vec<_>, _>>()?,
        Shape::Lane => {
            let mut it = args.into_iter();
            let (x, lane) = match (it.next(), it.next()) {
                (Some(x), Some(lane)) => (x, lane),
                _ => return Err(Error::invalid("simd_shuffle arity")),
            };
            vec![conv(x, ret)?, conv(lane, &Type::Scalar(Scalar::Ushort))?]
        }
        Shape::Select => {
            let mut it = args.into_iter();
            let (a, b, c) = match (it.next(), it.next(), it.next()) {
                (Some(a), Some(b), Some(c)) => (a, b, c),
                _ => return Err(Error::invalid("select arity")),
            };
            vec![conv(a, ret)?, conv(b, ret)?, c]
        }
        Shape::Extract | Shape::Insert => {
            let fixed = if shape == Shape::Extract { 1 } else { 2 };
            args.into_iter()
                .enumerate()
                .map(|(i, a)| {
                    if i < fixed {
                        conv(a, ret)
                    } else {
                        conv(a, &uint)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok((sname, out))
}
