//! The blend, color and channel mask state objects.

use super::*;

// Blend state.

pub(super) extern "C" fn blend_set_defaults(s: u64) {
    guard("nvnBlendStateSetDefaults", || {
        store(s, BLEND, blend_defaults())
    });
}

set_field!(
    blend_set_target,
    "nvnBlendStateSetBlendTarget",
    BLEND,
    BL_TARGET
);
set_field!(
    blend_set_overlap,
    "nvnBlendStateSetAdvancedOverlap",
    BLEND,
    BL_ADV_OVL
);
get_field!(
    blend_get_target,
    "nvnBlendStateGetBlendTarget",
    BLEND,
    BL_TARGET
);
get_field!(
    blend_get_advanced_mode,
    "nvnBlendStateGetAdvancedMode",
    BLEND,
    BL_ADV
);
get_field!(
    blend_get_overlap,
    "nvnBlendStateGetAdvancedOverlap",
    BLEND,
    BL_ADV_OVL
);

/// A mode above the last one is stored as none.
pub(super) extern "C" fn blend_set_advanced_mode(s: u64, mode: i32) {
    guard("nvnBlendStateSetAdvancedMode", || {
        let mode = if (0..=BLEND_ADVANCED_MODE_HSL_LUMINOSITY).contains(&mode) {
            mode
        } else {
            0
        };
        put(s, BLEND, BL_ADV, mode as u32)
    });
}

pub(super) extern "C" fn blend_set_func(s: u64, sr: i32, dr: i32, sa: i32, da: i32) {
    guard("nvnBlendStateSetBlendFunc", || {
        let mut w = load(s, BLEND)?;
        for (f, v) in [
            (BL_SRC_RGB, sr),
            (BL_DST_RGB, dr),
            (BL_SRC_A, sa),
            (BL_DST_A, da),
        ] {
            w = pack(w, f, v as u32);
        }
        store(s, BLEND, w)
    });
}

pub(super) extern "C" fn blend_set_equation(s: u64, er: i32, ea: i32) {
    guard("nvnBlendStateSetBlendEquation", || {
        let w = pack(
            pack(load(s, BLEND)?, BL_EQ_RGB, er as u32),
            BL_EQ_A,
            ea as u32,
        );
        store(s, BLEND, w)
    });
}

/// The flag is stored inverted, so that the zeroed state means premultiplied.
pub(super) extern "C" fn blend_set_premultiplied(s: u64, v: u32) {
    guard("nvnBlendStateSetAdvancedPremultipliedSrc", || {
        put(s, BLEND, BL_ADV_NPM, u32::from(!arg_bool(v)))
    });
}

/// The flag is stored inverted, so that the zeroed state means normalized.
pub(super) extern "C" fn blend_set_normalized(s: u64, v: u32) {
    guard("nvnBlendStateSetAdvancedNormalizedDst", || {
        put(s, BLEND, BL_ADV_DST_FREE, u32::from(!arg_bool(v)))
    });
}

pub(super) extern "C" fn blend_get_premultiplied(s: u64) -> u32 {
    guard("nvnBlendStateGetAdvancedPremultipliedSrc", || {
        Ok(ret_bool(get(s, BLEND, BL_ADV_NPM)? == 0))
    })
}

pub(super) extern "C" fn blend_get_normalized(s: u64) -> u32 {
    guard("nvnBlendStateGetAdvancedNormalizedDst", || {
        Ok(ret_bool(get(s, BLEND, BL_ADV_DST_FREE)? == 0))
    })
}

pub(super) extern "C" fn blend_get_func(s: u64, sr: u64, dr: u64, sa: u64, da: u64) {
    guard("nvnBlendStateGetBlendFunc", || {
        let w = load(s, BLEND)?;
        out_i32(sr, unpack(w, BL_SRC_RGB))?;
        out_i32(dr, unpack(w, BL_DST_RGB))?;
        out_i32(sa, unpack(w, BL_SRC_A))?;
        out_i32(da, unpack(w, BL_DST_A))
    });
}

pub(super) extern "C" fn blend_get_equation(s: u64, er: u64, ea: u64) {
    guard("nvnBlendStateGetBlendEquation", || {
        let w = load(s, BLEND)?;
        out_i32(er, unpack(w, BL_EQ_RGB))?;
        out_i32(ea, unpack(w, BL_EQ_A))
    });
}

// Color state.

pub(super) extern "C" fn color_set_defaults(s: u64) {
    guard("nvnColorStateSetDefaults", || {
        let w = pack(
            pack(0, COLOR_LOGIC_OP, LOGIC_OP_COPY),
            COLOR_ALPHA_FUNC,
            ALPHA_FUNC_ALWAYS,
        );
        store(s, COLOR, w)
    });
}

/// An index outside 0 to 7 changes nothing.
pub(super) extern "C" fn color_set_blend_enable(s: u64, i: i32, en: u32) {
    guard("nvnColorStateSetBlendEnable", || {
        if !(0..TARGETS).contains(&i) {
            return Ok(());
        }
        let bits = get(s, COLOR, COLOR_BLEND_EN)?;
        let bit = 1u32 << i;
        put(
            s,
            COLOR,
            COLOR_BLEND_EN,
            if arg_bool(en) {
                bits | bit
            } else {
                bits & !bit
            },
        )
    });
}

/// An index outside 0 to 7 reads as false.
pub(super) extern "C" fn color_get_blend_enable(s: u64, i: i32) -> u32 {
    guard("nvnColorStateGetBlendEnable", || {
        Ok(ret_bool(
            (0..TARGETS).contains(&i) && get(s, COLOR, COLOR_BLEND_EN)? >> i & 1 != 0,
        ))
    })
}

set_field!(
    color_set_alpha_test,
    "nvnColorStateSetAlphaTest",
    COLOR,
    COLOR_ALPHA_FUNC
);
set_field!(
    color_set_logic_op,
    "nvnColorStateSetLogicOp",
    COLOR,
    COLOR_LOGIC_OP
);
get_field!(
    color_get_alpha_test,
    "nvnColorStateGetAlphaTest",
    COLOR,
    COLOR_ALPHA_FUNC
);
get_field!(
    color_get_logic_op,
    "nvnColorStateGetLogicOp",
    COLOR,
    COLOR_LOGIC_OP
);

// Channel mask state.

pub(super) extern "C" fn mask_set_defaults(s: u64) {
    guard("nvnChannelMaskStateSetDefaults", || {
        Obj(s).set_u32(0, u32::MAX)
    });
}

/// An index outside 0 to 7 changes nothing.
pub(super) extern "C" fn mask_set(s: u64, i: i32, r: u32, g: u32, b: u32, a: u32) {
    guard("nvnChannelMaskStateSetChannelMask", || {
        if !(0..TARGETS).contains(&i) {
            return Ok(());
        }
        let m = u32::from(arg_bool(r))
            | u32::from(arg_bool(g)) << 1
            | u32::from(arg_bool(b)) << 2
            | u32::from(arg_bool(a)) << 3;
        let old = Obj(s).u32(0)?;
        Obj(s).set_u32(0, (old & !(0xfu32 << (4 * i))) | m << (4 * i))
    });
}

/// An index outside 0 to 7 reads as no channel. A null out-pointer is skipped.
pub(super) extern "C" fn mask_get(s: u64, i: i32, r: u64, g: u64, b: u64, a: u64) {
    guard("nvnChannelMaskStateGetChannelMask", || {
        let m = if (0..TARGETS).contains(&i) {
            Obj(s).u32(0)? >> (4 * i) & 0xf
        } else {
            0
        };
        out_bool(r, m & 1 != 0)?;
        out_bool(g, m & 2 != 0)?;
        out_bool(b, m & 4 != 0)?;
        out_bool(a, m & 8 != 0)
    });
}
