//! The block structs of uniform and storage buffers and the expressions that reach a field.
//!
//! A uniform block follows std140: every field takes whole 16 byte slots, so a scalar is followed by padding and an
//! array of scalars is an array of four component vectors read at `.x`. A storage block follows std430: the data is
//! a run time array of 32 bit words.

use super::types::scalar;
use crate::ir::agg::AggType;
use crate::props::{BufferDef, Field};

/// The name of the scalar type of a field.
fn base(f: &Field) -> Result<&'static str, String> {
    Ok(match scalar(f.ty) {
        t if t == AggType::FP32 => "float",
        t if t == AggType::S32 => "int",
        t if t == AggType::U32 => "uint",
        t if t == AggType::BOOL => "uint",
        _ => return Err("unported: 64 bit field of a buffer".into()),
    })
}

fn lanes(t: AggType) -> u32 {
    match t.0 & AggType::ELEMENT_COUNT_MASK {
        x if x == AggType::VECTOR2.0 => 2,
        x if x == AggType::VECTOR3.0 => 3,
        x if x == AggType::VECTOR4.0 => 4,
        _ => 1,
    }
}

fn is_array(f: &Field) -> bool {
    f.ty.0 & AggType::ARRAY.0 != 0
}

/// The text of the block struct `<name>_block`.
pub fn block(name: &str, b: &BufferDef) -> Result<String, String> {
    let mut s = format!("struct {name}_block {{\n");
    for (i, f) in b.fields.iter().enumerate() {
        let t = base(f)?;
        let n = lanes(f.ty);
        if b.layout == 1 {
            let vec = if n > 1 { n.to_string() } else { String::new() };
            let len = if is_array(f) && f.array_length > 0 {
                f.array_length
            } else {
                1
            };
            if is_array(f) || f.array_length == 0 {
                s.push_str(&format!("    {t}{vec} _m{i}[{len}];\n"));
            } else {
                s.push_str(&format!("    {t}{vec} _m{i};\n"));
            }
        } else if is_array(f) {
            s.push_str(&format!("    {t}4 _m{i}[{}];\n", f.array_length.max(1)));
        } else if n == 1 {
            s.push_str(&format!("    {t} _m{i};\n    {t} _p{i}[3];\n"));
        } else if n == 2 {
            s.push_str(&format!("    {t}2 _m{i};\n    {t}2 _p{i};\n"));
        } else {
            s.push_str(&format!("    {t}{n} _m{i};\n"));
        }
    }
    s.push_str("};\n");
    Ok(s)
}

/// The expression of a buffer field reached through the index operands (already MSL expressions).
///
/// `deref` is the pointer expression of the buffer, `field` the field number and `idx` the index operands.
pub fn access(deref: &str, b: &BufferDef, field: usize, idx: &[String]) -> Result<String, String> {
    let f = b.fields.get(field).ok_or("field out of range")?;
    let mut s = format!("(*{deref})._m{field}");
    let n = lanes(f.ty);
    let mut rest = idx;
    if b.layout == 1 {
        for i in rest {
            s.push_str(&format!("[{i}]"));
        }
        return Ok(s);
    }
    if is_array(f) {
        let first = rest.first().ok_or("array field without an index")?;
        s.push_str(&format!("[{first}]"));
        rest = &rest[1..];
        if n == 1 {
            if !rest.is_empty() {
                return Err("too many indices into an array of scalars".into());
            }
            s.push_str(".x");
            return Ok(s);
        }
    }
    for i in rest {
        s.push_str(&format!("[{i}]"));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(layout: i32, fields: Vec<Field>) -> BufferDef {
        BufferDef {
            layout,
            set: 0,
            binding: 3,
            name: "fp_c3".into(),
            fields,
        }
    }

    fn field(ty: AggType, n: i32) -> Field {
        Field {
            ty,
            array_length: n,
            name: "f".into(),
        }
    }

    #[test]
    fn std140_pads_scalars_and_arrays() {
        let b = buf(
            0,
            vec![
                field(AggType::U32, 1),
                field(AggType::ARRAY | AggType::U32, 8),
                field(AggType::ARRAY | AggType::VECTOR4 | AggType::FP32, 4096),
            ],
        );
        let t = block("fp_c0", &b).unwrap();
        assert!(t.contains("uint _m0;\n    uint _p0[3];"));
        assert!(t.contains("uint4 _m1[8];"));
        assert!(t.contains("float4 _m2[4096];"));
        let a = access("x", &b, 1, &["i".into()]).unwrap();
        assert_eq!(a, "(*x)._m1[i].x");
        let c = access("x", &b, 2, &["3".into(), "k".into()]).unwrap();
        assert_eq!(c, "(*x)._m2[3][k]");
    }

    #[test]
    fn std430_is_a_word_array() {
        let b = buf(1, vec![field(AggType::ARRAY | AggType::U32, 0)]);
        assert!(block("s0", &b).unwrap().contains("uint _m0[1];"));
        assert_eq!(access("x", &b, 0, &["i".into()]).unwrap(), "(*x)._m0[i]");
    }
}
