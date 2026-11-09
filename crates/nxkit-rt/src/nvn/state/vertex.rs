//! The vertex attribute and vertex stream state objects.

use super::*;

// Vertex attribute state.

pub(super) extern "C" fn va_set_defaults(s: u64) {
    guard("nvnVertexAttribStateSetDefaults", || store(s, VA, 0));
}

/// An offset outside the 19 bits is logged and stored cut to 19 bits.
pub(super) extern "C" fn va_set_format(s: u64, format: i32, offset: i64) {
    guard("nvnVertexAttribStateSetFormat", || {
        if !(0..ATTRIB_OFFSET_LIMIT).contains(&offset) {
            eprintln!(
                "[RED] NVN nvnVertexAttribStateSetFormat: the vertex attribute offset {offset} exceeds the packed range"
            );
        }
        let w = pack(
            pack(load(s, VA)?, VA_FMT, format as u32),
            VA_OFFSET,
            offset as u32,
        );
        store(s, VA, w)
    });
}

/// A null out-pointer is skipped.
pub(super) extern "C" fn va_get_format(s: u64, format: u64, offset: u64) {
    guard("nvnVertexAttribStateGetFormat", || {
        let w = load(s, VA)?;
        out_i32(format, unpack(w, VA_FMT))?;
        if offset != 0 {
            Obj(offset).set_i64(0, i64::from(unpack(w, VA_OFFSET)))?;
        }
        Ok(())
    });
}

set_field!(
    va_set_stream,
    "nvnVertexAttribStateSetStreamIndex",
    VA,
    VA_STREAM
);
get_field!(
    va_get_stream,
    "nvnVertexAttribStateGetStreamIndex",
    VA,
    VA_STREAM
);

// Vertex stream state.

pub(super) extern "C" fn vs_set_defaults(s: u64) {
    guard("nvnVertexStreamStateSetDefaults", || Obj(s).set_u64(0, 0));
}

/// The stride is stored as 32 bits.
pub(super) extern "C" fn vs_set_stride(s: u64, stride: i64) {
    guard("nvnVertexStreamStateSetStride", || {
        Obj(s).set_u32(VS_STRIDE, stride as u32)
    });
}

pub(super) extern "C" fn vs_get_stride(s: u64) -> i64 {
    guard("nvnVertexStreamStateGetStride", || {
        Ok(i64::from(Obj(s).u32(VS_STRIDE)?))
    })
}

pub(super) extern "C" fn vs_set_divisor(s: u64, divisor: i32) {
    guard("nvnVertexStreamStateSetDivisor", || {
        Obj(s).set_u32(VS_DIVISOR, divisor as u32)
    });
}

pub(super) extern "C" fn vs_get_divisor(s: u64) -> i32 {
    guard("nvnVertexStreamStateGetDivisor", || {
        Ok(Obj(s).u32(VS_DIVISOR)? as i32)
    })
}
