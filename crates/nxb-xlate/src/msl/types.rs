//! MSL type names, literals and casts between the scalar types of the structured program.

use crate::ir::agg::AggType;

/// The MSL name of an aggregate type (scalar or vector, no arrays).
pub fn msl_type(t: AggType) -> Result<String, String> {
    let base = match t.0 & AggType::ELEMENT_TYPE_MASK {
        x if x == AggType::BOOL.0 => "bool",
        x if x == AggType::FP32.0 => "float",
        x if x == AggType::S32.0 => "int",
        x if x == AggType::U32.0 => "uint",
        x if x == AggType::FP64.0 => return Err("unported: 64 bit float in MSL".into()),
        x if x == AggType::VOID.0 => return Ok("void".into()),
        _ => return Err(format!("unported: MSL type {:#x}", t.0)),
    };
    let n = match t.0 & AggType::ELEMENT_COUNT_MASK {
        0 => return Ok(base.into()),
        x if x == AggType::VECTOR2.0 => 2,
        x if x == AggType::VECTOR3.0 => 3,
        _ => 4,
    };
    Ok(format!("{base}{n}"))
}

/// The scalar type of an aggregate type.
pub fn scalar(t: AggType) -> AggType {
    AggType(t.0 & AggType::ELEMENT_TYPE_MASK)
}

/// True for the scalar float type.
pub fn is_float(t: AggType) -> bool {
    scalar(t) == AggType::FP32
}

/// A float literal for the raw 32 bits.
pub fn float_literal(bits: u32) -> String {
    let f = f32::from_bits(bits);
    if !f.is_finite() {
        return format!("as_type<float>({bits:#010x}u)");
    }
    let mut s = format!("{f:?}");
    if !s.contains('.') && !s.contains('e') {
        s.push_str(".0");
    }
    s.push('f');
    if f.is_sign_negative() {
        format!("({s})")
    } else {
        s
    }
}

/// A literal of a type for the raw 32 bits of an IR constant.
pub fn literal(t: AggType, bits: i32) -> String {
    match scalar(t) {
        x if x == AggType::BOOL => (bits != 0).to_string(),
        x if x == AggType::FP32 => float_literal(bits as u32),
        x if x == AggType::U32 => format!("{}u", bits as u32),
        _ => {
            if bits == i32::MIN {
                "(-2147483647 - 1)".to_string()
            } else if bits < 0 {
                format!("({bits})")
            } else {
                bits.to_string()
            }
        }
    }
}

/// The expression `s` of type `from` as a value of type `to` (bit casts, the Maxwell boolean is all ones).
pub fn cast(s: &str, from: AggType, to: AggType) -> String {
    let (f, t) = (scalar(from), scalar(to));
    if f == t {
        return s.to_string();
    }
    let ty = |x: AggType| match x {
        v if v == AggType::FP32 => "float",
        v if v == AggType::S32 => "int",
        v if v == AggType::U32 => "uint",
        _ => "bool",
    };
    if t == AggType::BOOL {
        return if f == AggType::FP32 {
            format!("(as_type<int>({s}) != 0)")
        } else {
            format!("({s} != 0)")
        };
    }
    if f == AggType::BOOL {
        return match t {
            v if v == AggType::S32 => format!("({s} ? -1 : 0)"),
            v if v == AggType::U32 => format!("({s} ? 0xffffffffu : 0u)"),
            _ => format!("as_type<float>({s} ? -1 : 0)"),
        };
    }
    format!("as_type<{}>({s})", ty(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_are_valid_msl() {
        assert_eq!(float_literal(1.0f32.to_bits()), "1.0f");
        assert_eq!(float_literal((-0.5f32).to_bits()), "(-0.5f)");
        assert_eq!(
            float_literal(f32::NAN.to_bits()),
            "as_type<float>(0x7fc00000u)"
        );
        assert_eq!(literal(AggType::S32, i32::MIN), "(-2147483647 - 1)");
        assert_eq!(literal(AggType::U32, -1), "4294967295u");
        assert_eq!(literal(AggType::BOOL, -1), "true");
    }

    #[test]
    fn casts_follow_the_boolean_convention() {
        assert_eq!(cast("x", AggType::S32, AggType::U32), "as_type<uint>(x)");
        assert_eq!(
            cast("x", AggType::FP32, AggType::BOOL),
            "(as_type<int>(x) != 0)"
        );
        assert_eq!(cast("b", AggType::BOOL, AggType::S32), "(b ? -1 : 0)");
        assert_eq!(cast("x", AggType::U32, AggType::U32), "x");
    }

    #[test]
    fn vector_types() {
        assert_eq!(
            msl_type(AggType(AggType::VECTOR4.0 | AggType::S32.0)).unwrap(),
            "int4"
        );
        assert!(msl_type(AggType::FP64).is_err());
    }
}
