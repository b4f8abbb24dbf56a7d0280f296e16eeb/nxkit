//! The byte and half word stores to shared memory as one Metal store, as an emitter choice.
//!
//! The front end turns a store of a byte or half word to shared memory into a call of `SharedStore8_<id>` or
//! `SharedStore16_<id>(byte offset, value)`. Its own body is a compare-and-swap loop on the word, because the shared
//! memory is an array of words. Metal's threadgroup memory takes byte stores, and each byte is its own memory location.
//! The emitter writes one store instead. Neighbouring bytes need no atomic. Two threads that store the same byte leave
//! one of the two values, as on the console. In a mesh workgroup the 32 threads store neighbouring primitive index
//! bytes, four to a word, and the loop retried about four times for each store.

use super::emit::{Em, R};

/// The bit width of the store that function `i` is, and the id of its shared memory.
fn parse(name: &str) -> Option<(u32, i32)> {
    let rest = name.strip_prefix("SharedStore")?;
    let (bits, id) = rest.split_once('_')?;
    let bits = bits.parse().ok().filter(|b| matches!(b, 8 | 16))?;
    Some((bits, id.parse().ok()?))
}

/// The body of function `i` when it is a shared store helper.
pub(super) fn body(em: &Em<'_>, i: usize) -> R<Option<String>> {
    let f = &em.info.functions[i];
    let Some((bits, id)) = (i != 0).then(|| parse(&f.name)).flatten() else {
        return Ok(None);
    };
    if f.in_arguments.len() != 2 || !f.out_arguments.is_empty() {
        return Err(format!("unported: {} with unexpected parameters", f.name));
    }
    if !em
        .props
        .properties
        .shared_memories
        .iter()
        .any(|m| m.id == id)
    {
        return Err(format!(
            "{} names a shared memory that is not declared",
            f.name
        ));
    }
    let space = if em.shared.spill {
        "device"
    } else {
        "threadgroup"
    };
    let (off, val) = ("uint(a0)", "uint(a1)");
    // A half word store is aligned. The narrow pointer may alias the words of the array.
    Ok(Some(if bits == 8 {
        format!("    (({space} uchar*)&sm{id}[0])[{off}] = uchar({val});\n")
    } else {
        format!(
            "    typedef ushort __attribute__((may_alias)) dm_ushort_alias;\n    (({space} dm_ushort_alias*)&sm{id}[0])[{off} >> 1u] = ushort({val});\n"
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn the_helper_names_are_parsed() {
        assert_eq!(parse("SharedStore8_0"), Some((8, 0)));
        assert_eq!(parse("SharedStore16_3"), Some((16, 3)));
        assert_eq!(parse("SharedStore32_0"), None);
        assert_eq!(parse("main"), None);
    }
}
