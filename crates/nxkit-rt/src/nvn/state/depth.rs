//! The depth stencil state object.

use super::*;

// Depth stencil state.

pub(super) extern "C" fn ds_set_defaults(s: u64) {
    guard("nvnDepthStencilStateSetDefaults", || {
        store(s, DS, depth_stencil_defaults())
    });
}

set_flag!(
    ds_set_depth_test,
    "nvnDepthStencilStateSetDepthTestEnable",
    DS,
    DS_DT
);
set_flag!(
    ds_set_depth_write,
    "nvnDepthStencilStateSetDepthWriteEnable",
    DS,
    DS_DW
);
set_flag!(
    ds_set_stencil_test,
    "nvnDepthStencilStateSetStencilTestEnable",
    DS,
    DS_ST
);
set_field!(
    ds_set_depth_func,
    "nvnDepthStencilStateSetDepthFunc",
    DS,
    DS_DF
);
get_flag!(
    ds_get_depth_test,
    "nvnDepthStencilStateGetDepthTestEnable",
    DS,
    DS_DT
);
get_flag!(
    ds_get_depth_write,
    "nvnDepthStencilStateGetDepthWriteEnable",
    DS,
    DS_DW
);
get_flag!(
    ds_get_stencil_test,
    "nvnDepthStencilStateGetStencilTestEnable",
    DS,
    DS_ST
);
get_field!(
    ds_get_depth_func,
    "nvnDepthStencilStateGetDepthFunc",
    DS,
    DS_DF
);

/// Sets the function of the front face, of the back face, or of both.
pub(super) extern "C" fn ds_set_stencil_func(s: u64, face: i32, func: i32) {
    guard("nvnDepthStencilStateSetStencilFunc", || {
        let mut w = load(s, DS)?;
        if face & FACE_FRONT != 0 {
            w = pack(w, DS_SF_F, func as u32);
        }
        if face & FACE_BACK != 0 {
            w = pack(w, DS_SF_B, func as u32);
        }
        store(s, DS, w)
    });
}

/// Sets the operations of the front face, of the back face, or of both.
pub(super) extern "C" fn ds_set_stencil_op(s: u64, face: i32, fail: i32, zfail: i32, zpass: i32) {
    guard("nvnDepthStencilStateSetStencilOp", || {
        let mut w = load(s, DS)?;
        if face & FACE_FRONT != 0 {
            for (f, v) in [(DS_FAIL_F, fail), (DS_ZFAIL_F, zfail), (DS_ZPASS_F, zpass)] {
                w = pack(w, f, v as u32);
            }
        }
        if face & FACE_BACK != 0 {
            for (f, v) in [(DS_FAIL_B, fail), (DS_ZFAIL_B, zfail), (DS_ZPASS_B, zpass)] {
                w = pack(w, f, v as u32);
            }
        }
        store(s, DS, w)
    });
}

/// Any face but the back face reads the front face.
pub(super) extern "C" fn ds_get_stencil_func(s: u64, face: i32) -> i32 {
    guard("nvnDepthStencilStateGetStencilFunc", || {
        let f = if face == FACE_BACK { DS_SF_B } else { DS_SF_F };
        Ok(get(s, DS, f)? as i32)
    })
}

/// Any face but the back face reads the front face. A null out-pointer is skipped.
pub(super) extern "C" fn ds_get_stencil_op(s: u64, face: i32, fail: u64, zfail: u64, zpass: u64) {
    guard("nvnDepthStencilStateGetStencilOp", || {
        let w = load(s, DS)?;
        let (a, b, c) = if face == FACE_BACK {
            (DS_FAIL_B, DS_ZFAIL_B, DS_ZPASS_B)
        } else {
            (DS_FAIL_F, DS_ZFAIL_F, DS_ZPASS_F)
        };
        out_i32(fail, unpack(w, a))?;
        out_i32(zfail, unpack(w, b))?;
        out_i32(zpass, unpack(w, c))
    });
}
