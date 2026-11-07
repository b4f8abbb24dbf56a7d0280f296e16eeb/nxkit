//! The state group: the blend, color, channel mask, depth stencil, multisample, polygon, vertex attribute and vertex
//! stream state objects.
//!
//! A state object is plain data in guest memory with no magic and no host data. The functions pack the values in place,
//! little-endian, with the bit-fields filled from the lowest bit. A game does not read the packed bytes. It calls the
//! getters, so the layout is private to this file and to the readers in `state/read.rs`.
//!
//! | object | bytes used | fields, lowest bit first |
//! | --- | --- | --- |
//! | color | 4 | blend enable 8, logic op 4, alpha func 4 |
//! | channel mask | 4 | 4 bits per target, red 1, green 2, blue 4, alpha 8 |
//! | blend | 8 | target 3, source and destination of color and of alpha 7 each, two equations 3 each, advanced mode 6, not premultiplied 1, overlap 2, destination not normalized 1 |
//! | multisample | 10 | one byte each: enable, samples, alpha to coverage, dither, raster samples, coverage modulation, coverage to color enable and output, sample locations enable and grid |
//! | polygon | 4 | cull face 2, front face 1, mode 2, offset enables 3 |
//! | depth stencil | 8 | depth test 1, depth write 1, depth func 4, stencil test 1, function front 4, function back 4, then fail, depth fail and pass 4 each for the front and then for the back |
//! | vertex attribute | 4 | format 8, offset 19, stream 5 |
//! | vertex stream | 8 | stride 32, divisor 32 |
//!
//! The header sizes of the objects are larger (a multisample state is 0x18 bytes). The functions touch the bytes of the
//! table only. The command buffer group reads a state with [`blend`], [`color`], [`channel_mask`], [`depth_stencil`],
//! [`multisample`], [`polygon`], [`vertex_attrib`] and [`vertex_stream`].
//!
//! Defaults, the bounds of the indexes and the getters follow `dm/gfx/nvn/hle_nvn.c` of nxnative (checked 2026-11-07).
//! The NVN enum values are those of `dm/gfx/nvn/include/nvn.h`.

use super::abi::{AbiResult, Obj, arg_bool, guard, log_once, ret_bool, table};

mod read;

pub(crate) use read::*;

/// A bit-field: the lowest bit and the width.
type Field = (u32, u32);

const LOGIC_OP_COPY: u32 = 3;
const ALPHA_FUNC_ALWAYS: u32 = 8;
const BLEND_FUNC_ZERO: u32 = 1;
const BLEND_FUNC_ONE: u32 = 2;
const BLEND_EQUATION_ADD: u32 = 1;
const BLEND_ADVANCED_MODE_HSL_LUMINOSITY: i32 = 0x32;
const FACE_FRONT: i32 = 1;
const FACE_BACK: i32 = 2;
const FRONT_FACE_CCW: u32 = 1;
const POLYGON_MODE_FILL: u32 = 2;
const DEPTH_FUNC_LESS: u32 = 2;
const STENCIL_FUNC_ALWAYS: u32 = 8;
const STENCIL_OP_KEEP: u32 = 1;

/// The number of render targets of a color state and a channel mask state.
const TARGETS: i32 = 8;
/// The vertex attribute offset is 19 bits wide.
const ATTRIB_OFFSET_LIMIT: i64 = 1 << 19;

const COLOR: u64 = 4;
const COLOR_BLEND_EN: Field = (0, 8);
const COLOR_LOGIC_OP: Field = (8, 4);
const COLOR_ALPHA_FUNC: Field = (12, 4);

const BLEND: u64 = 8;
const BL_TARGET: Field = (0, 3);
const BL_SRC_RGB: Field = (3, 7);
const BL_DST_RGB: Field = (10, 7);
const BL_SRC_A: Field = (17, 7);
const BL_DST_A: Field = (24, 7);
const BL_EQ_RGB: Field = (31, 3);
const BL_EQ_A: Field = (34, 3);
const BL_ADV: Field = (37, 6);
const BL_ADV_NPM: Field = (43, 1);
const BL_ADV_OVL: Field = (44, 2);
const BL_ADV_DST_FREE: Field = (46, 1);

const POLY: u64 = 4;
const PO_CULL: Field = (0, 2);
const PO_FRONT: Field = (2, 1);
const PO_MODE: Field = (3, 2);
const PO_OFFSET: Field = (5, 3);

const DS: u64 = 8;
const DS_DT: Field = (0, 1);
const DS_DW: Field = (1, 1);
const DS_DF: Field = (2, 4);
const DS_ST: Field = (6, 1);
const DS_SF_F: Field = (7, 4);
const DS_SF_B: Field = (11, 4);
const DS_FAIL_F: Field = (15, 4);
const DS_ZFAIL_F: Field = (19, 4);
const DS_ZPASS_F: Field = (23, 4);
const DS_FAIL_B: Field = (27, 4);
const DS_ZFAIL_B: Field = (31, 4);
const DS_ZPASS_B: Field = (35, 4);

const VA: u64 = 4;
const VA_FMT: Field = (0, 8);
const VA_OFFSET: Field = (8, 19);
const VA_STREAM: Field = (27, 5);

const MS_SIZE: u64 = 10;
const MS_ENABLE: u64 = 0;
const MS_SAMPLES: u64 = 1;
const MS_A2C: u64 = 2;
const MS_A2C_DITHER: u64 = 3;
const MS_RASTER: u64 = 4;
const MS_COV_MOD: u64 = 5;
const MS_C2C_EN: u64 = 6;
const MS_C2C_OUT: u64 = 7;
const MS_SL_EN: u64 = 8;
const MS_SL_GRID: u64 = 9;

const VS_STRIDE: u64 = 0;
const VS_DIVISOR: u64 = 4;

fn mask(f: Field) -> u64 {
    ((1u64 << f.1) - 1) << f.0
}

fn pack(word: u64, f: Field, v: u32) -> u64 {
    (word & !mask(f)) | ((u64::from(v) << f.0) & mask(f))
}

fn unpack(word: u64, f: Field) -> u32 {
    ((word & mask(f)) >> f.0) as u32
}

/// The packed word of an object that is `bytes` long, 4 or 8.
fn load(s: u64, bytes: u64) -> AbiResult<u64> {
    if bytes == 4 {
        Ok(u64::from(Obj(s).u32(0)?))
    } else {
        Obj(s).u64(0)
    }
}

fn store(s: u64, bytes: u64, word: u64) -> AbiResult<()> {
    if bytes == 4 {
        Obj(s).set_u32(0, word as u32)
    } else {
        Obj(s).set_u64(0, word)
    }
}

fn get(s: u64, bytes: u64, f: Field) -> AbiResult<u32> {
    Ok(unpack(load(s, bytes)?, f))
}

fn put(s: u64, bytes: u64, f: Field, v: u32) -> AbiResult<()> {
    store(s, bytes, pack(load(s, bytes)?, f, v))
}

fn byte(s: u64, off: u64) -> AbiResult<u8> {
    let mut b = [0u8];
    Obj(s).read(off, &mut b)?;
    Ok(b[0])
}

fn set_byte(s: u64, off: u64, v: u8) -> AbiResult<()> {
    Obj(s).write(off, &[v])
}

/// Writes a boolean to an out-pointer. A null pointer is skipped.
fn out_bool(p: u64, v: bool) -> AbiResult<()> {
    if p == 0 {
        return Ok(());
    }
    Obj(p).write(0, &[u8::from(v)])
}

/// Writes an enum value to an out-pointer. A null pointer is skipped.
fn out_i32(p: u64, v: u32) -> AbiResult<()> {
    if p == 0 {
        return Ok(());
    }
    Obj(p).set_i32(0, v as i32)
}

fn blend_defaults() -> u64 {
    [
        (BL_SRC_RGB, BLEND_FUNC_ONE),
        (BL_DST_RGB, BLEND_FUNC_ZERO),
        (BL_SRC_A, BLEND_FUNC_ONE),
        (BL_DST_A, BLEND_FUNC_ZERO),
        (BL_EQ_RGB, BLEND_EQUATION_ADD),
        (BL_EQ_A, BLEND_EQUATION_ADD),
    ]
    .into_iter()
    .fold(0, |w, (f, v)| pack(w, f, v))
}

fn depth_stencil_defaults() -> u64 {
    [
        (DS_DW, 1),
        (DS_DF, DEPTH_FUNC_LESS),
        (DS_SF_F, STENCIL_FUNC_ALWAYS),
        (DS_SF_B, STENCIL_FUNC_ALWAYS),
        (DS_FAIL_F, STENCIL_OP_KEEP),
        (DS_ZFAIL_F, STENCIL_OP_KEEP),
        (DS_ZPASS_F, STENCIL_OP_KEEP),
        (DS_FAIL_B, STENCIL_OP_KEEP),
        (DS_ZFAIL_B, STENCIL_OP_KEEP),
        (DS_ZPASS_B, STENCIL_OP_KEEP),
    ]
    .into_iter()
    .fold(0, |w, (f, v)| pack(w, f, v))
}

/// A setter of one bit-field. The value is cut to the width of the field.
macro_rules! set_field {
    ($fname:ident, $name:literal, $bytes:expr, $field:expr) => {
        extern "C" fn $fname(s: u64, v: i32) {
            guard($name, || put(s, $bytes, $field, v as u32))
        }
    };
}

/// A setter of one bit-field that holds an NVNboolean.
macro_rules! set_flag {
    ($fname:ident, $name:literal, $bytes:expr, $field:expr) => {
        extern "C" fn $fname(s: u64, v: u32) {
            guard($name, || put(s, $bytes, $field, u32::from(arg_bool(v))))
        }
    };
}

/// A getter of one bit-field that returns an enum or an integer.
macro_rules! get_field {
    ($fname:ident, $name:literal, $bytes:expr, $field:expr) => {
        extern "C" fn $fname(s: u64) -> i32 {
            guard($name, || Ok(get(s, $bytes, $field)? as i32))
        }
    };
}

/// A getter of one bit-field that returns an NVNboolean.
macro_rules! get_flag {
    ($fname:ident, $name:literal, $bytes:expr, $field:expr) => {
        extern "C" fn $fname(s: u64) -> u32 {
            guard($name, || Ok(ret_bool(get(s, $bytes, $field)? != 0)))
        }
    };
}

/// A setter of one byte of the multisample state.
macro_rules! ms_set {
    ($fname:ident, $name:literal, $off:expr, int) => {
        extern "C" fn $fname(s: u64, v: i32) {
            guard($name, || set_byte(s, $off, v as u8))
        }
    };
    ($fname:ident, $name:literal, $off:expr, flag) => {
        extern "C" fn $fname(s: u64, v: u32) {
            guard($name, || set_byte(s, $off, u8::from(arg_bool(v))))
        }
    };
}

/// A getter of one byte of the multisample state.
macro_rules! ms_get {
    ($fname:ident, $name:literal, $off:expr, int) => {
        extern "C" fn $fname(s: u64) -> i32 {
            guard($name, || Ok(i32::from(byte(s, $off)?)))
        }
    };
    ($fname:ident, $name:literal, $off:expr, flag) => {
        extern "C" fn $fname(s: u64) -> u32 {
            guard($name, || Ok(u32::from(byte(s, $off)?)))
        }
    };
}

// Blend state.

extern "C" fn blend_set_defaults(s: u64) {
    guard("nvnBlendStateSetDefaults", || store(s, BLEND, blend_defaults()));
}

set_field!(blend_set_target, "nvnBlendStateSetBlendTarget", BLEND, BL_TARGET);
set_field!(blend_set_overlap, "nvnBlendStateSetAdvancedOverlap", BLEND, BL_ADV_OVL);
get_field!(blend_get_target, "nvnBlendStateGetBlendTarget", BLEND, BL_TARGET);
get_field!(blend_get_advanced_mode, "nvnBlendStateGetAdvancedMode", BLEND, BL_ADV);
get_field!(blend_get_overlap, "nvnBlendStateGetAdvancedOverlap", BLEND, BL_ADV_OVL);

/// A mode above the last one is stored as none.
extern "C" fn blend_set_advanced_mode(s: u64, mode: i32) {
    guard("nvnBlendStateSetAdvancedMode", || {
        let mode = if (0..=BLEND_ADVANCED_MODE_HSL_LUMINOSITY).contains(&mode) { mode } else { 0 };
        put(s, BLEND, BL_ADV, mode as u32)
    });
}

extern "C" fn blend_set_func(s: u64, sr: i32, dr: i32, sa: i32, da: i32) {
    guard("nvnBlendStateSetBlendFunc", || {
        let mut w = load(s, BLEND)?;
        for (f, v) in [(BL_SRC_RGB, sr), (BL_DST_RGB, dr), (BL_SRC_A, sa), (BL_DST_A, da)] {
            w = pack(w, f, v as u32);
        }
        store(s, BLEND, w)
    });
}

extern "C" fn blend_set_equation(s: u64, er: i32, ea: i32) {
    guard("nvnBlendStateSetBlendEquation", || {
        let w = pack(pack(load(s, BLEND)?, BL_EQ_RGB, er as u32), BL_EQ_A, ea as u32);
        store(s, BLEND, w)
    });
}

/// The flag is stored inverted, so that the zeroed state means premultiplied.
extern "C" fn blend_set_premultiplied(s: u64, v: u32) {
    guard("nvnBlendStateSetAdvancedPremultipliedSrc", || {
        put(s, BLEND, BL_ADV_NPM, u32::from(!arg_bool(v)))
    });
}

/// The flag is stored inverted, so that the zeroed state means normalized.
extern "C" fn blend_set_normalized(s: u64, v: u32) {
    guard("nvnBlendStateSetAdvancedNormalizedDst", || {
        put(s, BLEND, BL_ADV_DST_FREE, u32::from(!arg_bool(v)))
    });
}

extern "C" fn blend_get_premultiplied(s: u64) -> u32 {
    guard("nvnBlendStateGetAdvancedPremultipliedSrc", || {
        Ok(ret_bool(get(s, BLEND, BL_ADV_NPM)? == 0))
    })
}

extern "C" fn blend_get_normalized(s: u64) -> u32 {
    guard("nvnBlendStateGetAdvancedNormalizedDst", || {
        Ok(ret_bool(get(s, BLEND, BL_ADV_DST_FREE)? == 0))
    })
}

extern "C" fn blend_get_func(s: u64, sr: u64, dr: u64, sa: u64, da: u64) {
    guard("nvnBlendStateGetBlendFunc", || {
        let w = load(s, BLEND)?;
        out_i32(sr, unpack(w, BL_SRC_RGB))?;
        out_i32(dr, unpack(w, BL_DST_RGB))?;
        out_i32(sa, unpack(w, BL_SRC_A))?;
        out_i32(da, unpack(w, BL_DST_A))
    });
}

extern "C" fn blend_get_equation(s: u64, er: u64, ea: u64) {
    guard("nvnBlendStateGetBlendEquation", || {
        let w = load(s, BLEND)?;
        out_i32(er, unpack(w, BL_EQ_RGB))?;
        out_i32(ea, unpack(w, BL_EQ_A))
    });
}

// Color state.

extern "C" fn color_set_defaults(s: u64) {
    guard("nvnColorStateSetDefaults", || {
        let w = pack(pack(0, COLOR_LOGIC_OP, LOGIC_OP_COPY), COLOR_ALPHA_FUNC, ALPHA_FUNC_ALWAYS);
        store(s, COLOR, w)
    });
}

/// An index outside 0 to 7 changes nothing.
extern "C" fn color_set_blend_enable(s: u64, i: i32, en: u32) {
    guard("nvnColorStateSetBlendEnable", || {
        if !(0..TARGETS).contains(&i) {
            return Ok(());
        }
        let bits = get(s, COLOR, COLOR_BLEND_EN)?;
        let bit = 1u32 << i;
        put(s, COLOR, COLOR_BLEND_EN, if arg_bool(en) { bits | bit } else { bits & !bit })
    });
}

/// An index outside 0 to 7 reads as false.
extern "C" fn color_get_blend_enable(s: u64, i: i32) -> u32 {
    guard("nvnColorStateGetBlendEnable", || {
        Ok(ret_bool((0..TARGETS).contains(&i) && get(s, COLOR, COLOR_BLEND_EN)? >> i & 1 != 0))
    })
}

set_field!(color_set_alpha_test, "nvnColorStateSetAlphaTest", COLOR, COLOR_ALPHA_FUNC);
set_field!(color_set_logic_op, "nvnColorStateSetLogicOp", COLOR, COLOR_LOGIC_OP);
get_field!(color_get_alpha_test, "nvnColorStateGetAlphaTest", COLOR, COLOR_ALPHA_FUNC);
get_field!(color_get_logic_op, "nvnColorStateGetLogicOp", COLOR, COLOR_LOGIC_OP);

// Channel mask state.

extern "C" fn mask_set_defaults(s: u64) {
    guard("nvnChannelMaskStateSetDefaults", || Obj(s).set_u32(0, u32::MAX));
}

/// An index outside 0 to 7 changes nothing.
extern "C" fn mask_set(s: u64, i: i32, r: u32, g: u32, b: u32, a: u32) {
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
extern "C" fn mask_get(s: u64, i: i32, r: u64, g: u64, b: u64, a: u64) {
    guard("nvnChannelMaskStateGetChannelMask", || {
        let m = if (0..TARGETS).contains(&i) { Obj(s).u32(0)? >> (4 * i) & 0xf } else { 0 };
        out_bool(r, m & 1 != 0)?;
        out_bool(g, m & 2 != 0)?;
        out_bool(b, m & 4 != 0)?;
        out_bool(a, m & 8 != 0)
    });
}

// Multisample state.

extern "C" fn ms_set_defaults(s: u64) {
    guard("nvnMultisampleStateSetDefaults", || {
        Obj(s).zero(MS_SIZE)?;
        set_byte(s, MS_ENABLE, 1)
    });
}

ms_set!(ms_set_enable, "nvnMultisampleStateSetMultisampleEnable", MS_ENABLE, flag);
ms_set!(ms_set_samples, "nvnMultisampleStateSetSamples", MS_SAMPLES, int);
ms_set!(ms_set_a2c, "nvnMultisampleStateSetAlphaToCoverageEnable", MS_A2C, flag);
ms_set!(ms_set_a2c_dither, "nvnMultisampleStateSetAlphaToCoverageDither", MS_A2C_DITHER, flag);
ms_set!(ms_set_raster, "nvnMultisampleStateSetRasterSamples", MS_RASTER, int);
ms_set!(ms_set_cov_mod, "nvnMultisampleStateSetCoverageModulationMode", MS_COV_MOD, int);
ms_set!(ms_set_c2c_en, "nvnMultisampleStateSetCoverageToColorEnable", MS_C2C_EN, flag);
ms_set!(ms_set_c2c_out, "nvnMultisampleStateSetCoverageToColorOutput", MS_C2C_OUT, int);
ms_set!(ms_set_sl_en, "nvnMultisampleStateSetSampleLocationsEnable", MS_SL_EN, flag);
ms_set!(ms_set_sl_grid, "nvnMultisampleStateSetSampleLocationsGridEnable", MS_SL_GRID, flag);
ms_get!(ms_get_enable, "nvnMultisampleStateGetMultisampleEnable", MS_ENABLE, flag);
ms_get!(ms_get_samples, "nvnMultisampleStateGetSamples", MS_SAMPLES, int);
ms_get!(ms_get_a2c, "nvnMultisampleStateGetAlphaToCoverageEnable", MS_A2C, flag);
ms_get!(ms_get_a2c_dither, "nvnMultisampleStateGetAlphaToCoverageDither", MS_A2C_DITHER, flag);
ms_get!(ms_get_raster, "nvnMultisampleStateGetRasterSamples", MS_RASTER, int);
ms_get!(ms_get_cov_mod, "nvnMultisampleStateGetCoverageModulationMode", MS_COV_MOD, int);
ms_get!(ms_get_c2c_en, "nvnMultisampleStateGetCoverageToColorEnable", MS_C2C_EN, flag);
ms_get!(ms_get_c2c_out, "nvnMultisampleStateGetCoverageToColorOutput", MS_C2C_OUT, int);
ms_get!(ms_get_sl_en, "nvnMultisampleStateGetSampleLocationsEnable", MS_SL_EN, flag);
ms_get!(ms_get_sl_grid, "nvnMultisampleStateGetSampleLocationsGridEnable", MS_SL_GRID, flag);

/// The programmable sample positions are not kept, as in nxnative.
extern "C" fn ms_set_sample_locations(_s: u64, _start: i32, _n: i32, _v: u64) {
    log_once(
        "nvnMultisampleStateSetSampleLocations",
        format_args!("nvnMultisampleStateSetSampleLocations: the sample positions are ignored"),
    );
}

// Polygon state.

extern "C" fn poly_set_defaults(s: u64) {
    guard("nvnPolygonStateSetDefaults", || {
        let w = pack(pack(0, PO_FRONT, FRONT_FACE_CCW), PO_MODE, POLYGON_MODE_FILL);
        store(s, POLY, w)
    });
}

set_field!(poly_set_cull, "nvnPolygonStateSetCullFace", POLY, PO_CULL);
set_field!(poly_set_front, "nvnPolygonStateSetFrontFace", POLY, PO_FRONT);
set_field!(poly_set_mode, "nvnPolygonStateSetPolygonMode", POLY, PO_MODE);
set_field!(poly_set_offset, "nvnPolygonStateSetPolygonOffsetEnables", POLY, PO_OFFSET);
get_field!(poly_get_cull, "nvnPolygonStateGetCullFace", POLY, PO_CULL);
get_field!(poly_get_front, "nvnPolygonStateGetFrontFace", POLY, PO_FRONT);
get_field!(poly_get_mode, "nvnPolygonStateGetPolygonMode", POLY, PO_MODE);
get_field!(poly_get_offset, "nvnPolygonStateGetPolygonOffsetEnables", POLY, PO_OFFSET);

// Depth stencil state.

extern "C" fn ds_set_defaults(s: u64) {
    guard("nvnDepthStencilStateSetDefaults", || store(s, DS, depth_stencil_defaults()));
}

set_flag!(ds_set_depth_test, "nvnDepthStencilStateSetDepthTestEnable", DS, DS_DT);
set_flag!(ds_set_depth_write, "nvnDepthStencilStateSetDepthWriteEnable", DS, DS_DW);
set_flag!(ds_set_stencil_test, "nvnDepthStencilStateSetStencilTestEnable", DS, DS_ST);
set_field!(ds_set_depth_func, "nvnDepthStencilStateSetDepthFunc", DS, DS_DF);
get_flag!(ds_get_depth_test, "nvnDepthStencilStateGetDepthTestEnable", DS, DS_DT);
get_flag!(ds_get_depth_write, "nvnDepthStencilStateGetDepthWriteEnable", DS, DS_DW);
get_flag!(ds_get_stencil_test, "nvnDepthStencilStateGetStencilTestEnable", DS, DS_ST);
get_field!(ds_get_depth_func, "nvnDepthStencilStateGetDepthFunc", DS, DS_DF);

/// Sets the function of the front face, of the back face, or of both.
extern "C" fn ds_set_stencil_func(s: u64, face: i32, func: i32) {
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
extern "C" fn ds_set_stencil_op(s: u64, face: i32, fail: i32, zfail: i32, zpass: i32) {
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
extern "C" fn ds_get_stencil_func(s: u64, face: i32) -> i32 {
    guard("nvnDepthStencilStateGetStencilFunc", || {
        let f = if face == FACE_BACK { DS_SF_B } else { DS_SF_F };
        Ok(get(s, DS, f)? as i32)
    })
}

/// Any face but the back face reads the front face. A null out-pointer is skipped.
extern "C" fn ds_get_stencil_op(s: u64, face: i32, fail: u64, zfail: u64, zpass: u64) {
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

// Vertex attribute state.

extern "C" fn va_set_defaults(s: u64) {
    guard("nvnVertexAttribStateSetDefaults", || store(s, VA, 0));
}

/// An offset outside the 19 bits is logged and stored cut to 19 bits, as in nxnative.
extern "C" fn va_set_format(s: u64, format: i32, offset: i64) {
    guard("nvnVertexAttribStateSetFormat", || {
        if !(0..ATTRIB_OFFSET_LIMIT).contains(&offset) {
            eprintln!(
                "[RED] NVN nvnVertexAttribStateSetFormat: the vertex attribute offset {offset} exceeds the packed range"
            );
        }
        let w = pack(pack(load(s, VA)?, VA_FMT, format as u32), VA_OFFSET, offset as u32);
        store(s, VA, w)
    });
}

/// A null out-pointer is skipped.
extern "C" fn va_get_format(s: u64, format: u64, offset: u64) {
    guard("nvnVertexAttribStateGetFormat", || {
        let w = load(s, VA)?;
        out_i32(format, unpack(w, VA_FMT))?;
        if offset != 0 {
            Obj(offset).set_i64(0, i64::from(unpack(w, VA_OFFSET)))?;
        }
        Ok(())
    });
}

set_field!(va_set_stream, "nvnVertexAttribStateSetStreamIndex", VA, VA_STREAM);
get_field!(va_get_stream, "nvnVertexAttribStateGetStreamIndex", VA, VA_STREAM);

// Vertex stream state.

extern "C" fn vs_set_defaults(s: u64) {
    guard("nvnVertexStreamStateSetDefaults", || Obj(s).set_u64(0, 0));
}

/// The stride is stored as 32 bits.
extern "C" fn vs_set_stride(s: u64, stride: i64) {
    guard("nvnVertexStreamStateSetStride", || Obj(s).set_u32(VS_STRIDE, stride as u32));
}

extern "C" fn vs_get_stride(s: u64) -> i64 {
    guard("nvnVertexStreamStateGetStride", || Ok(i64::from(Obj(s).u32(VS_STRIDE)?)))
}

extern "C" fn vs_set_divisor(s: u64, divisor: i32) {
    guard("nvnVertexStreamStateSetDivisor", || Obj(s).set_u32(VS_DIVISOR, divisor as u32));
}

extern "C" fn vs_get_divisor(s: u64) -> i32 {
    guard("nvnVertexStreamStateGetDivisor", || Ok(Obj(s).u32(VS_DIVISOR)? as i32))
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnBlendStateSetDefaults" => blend_set_defaults as extern "C" fn(u64),
        "nvnBlendStateSetBlendTarget" => blend_set_target as extern "C" fn(u64, i32),
        "nvnBlendStateSetBlendFunc" => blend_set_func as extern "C" fn(u64, i32, i32, i32, i32),
        "nvnBlendStateSetBlendEquation" => blend_set_equation as extern "C" fn(u64, i32, i32),
        "nvnBlendStateSetAdvancedMode" => blend_set_advanced_mode as extern "C" fn(u64, i32),
        "nvnBlendStateSetAdvancedOverlap" => blend_set_overlap as extern "C" fn(u64, i32),
        "nvnBlendStateSetAdvancedPremultipliedSrc" => blend_set_premultiplied as extern "C" fn(u64, u32),
        "nvnBlendStateSetAdvancedNormalizedDst" => blend_set_normalized as extern "C" fn(u64, u32),
        "nvnBlendStateGetBlendTarget" => blend_get_target as extern "C" fn(u64) -> i32,
        "nvnBlendStateGetBlendFunc" => blend_get_func as extern "C" fn(u64, u64, u64, u64, u64),
        "nvnBlendStateGetBlendEquation" => blend_get_equation as extern "C" fn(u64, u64, u64),
        "nvnBlendStateGetAdvancedMode" => blend_get_advanced_mode as extern "C" fn(u64) -> i32,
        "nvnBlendStateGetAdvancedOverlap" => blend_get_overlap as extern "C" fn(u64) -> i32,
        "nvnBlendStateGetAdvancedPremultipliedSrc" => blend_get_premultiplied as extern "C" fn(u64) -> u32,
        "nvnBlendStateGetAdvancedNormalizedDst" => blend_get_normalized as extern "C" fn(u64) -> u32,
        "nvnColorStateSetDefaults" => color_set_defaults as extern "C" fn(u64),
        "nvnColorStateSetBlendEnable" => color_set_blend_enable as extern "C" fn(u64, i32, u32),
        "nvnColorStateSetAlphaTest" => color_set_alpha_test as extern "C" fn(u64, i32),
        "nvnColorStateSetLogicOp" => color_set_logic_op as extern "C" fn(u64, i32),
        "nvnColorStateGetBlendEnable" => color_get_blend_enable as extern "C" fn(u64, i32) -> u32,
        "nvnColorStateGetAlphaTest" => color_get_alpha_test as extern "C" fn(u64) -> i32,
        "nvnColorStateGetLogicOp" => color_get_logic_op as extern "C" fn(u64) -> i32,
        "nvnChannelMaskStateSetDefaults" => mask_set_defaults as extern "C" fn(u64),
        "nvnChannelMaskStateSetChannelMask" => mask_set as extern "C" fn(u64, i32, u32, u32, u32, u32),
        "nvnChannelMaskStateGetChannelMask" => mask_get as extern "C" fn(u64, i32, u64, u64, u64, u64),
        "nvnMultisampleStateSetDefaults" => ms_set_defaults as extern "C" fn(u64),
        "nvnMultisampleStateSetMultisampleEnable" => ms_set_enable as extern "C" fn(u64, u32),
        "nvnMultisampleStateSetSamples" => ms_set_samples as extern "C" fn(u64, i32),
        "nvnMultisampleStateSetAlphaToCoverageEnable" => ms_set_a2c as extern "C" fn(u64, u32),
        "nvnMultisampleStateSetAlphaToCoverageDither" => ms_set_a2c_dither as extern "C" fn(u64, u32),
        "nvnMultisampleStateSetRasterSamples" => ms_set_raster as extern "C" fn(u64, i32),
        "nvnMultisampleStateSetCoverageModulationMode" => ms_set_cov_mod as extern "C" fn(u64, i32),
        "nvnMultisampleStateSetCoverageToColorEnable" => ms_set_c2c_en as extern "C" fn(u64, u32),
        "nvnMultisampleStateSetCoverageToColorOutput" => ms_set_c2c_out as extern "C" fn(u64, i32),
        "nvnMultisampleStateSetSampleLocationsEnable" => ms_set_sl_en as extern "C" fn(u64, u32),
        "nvnMultisampleStateSetSampleLocationsGridEnable" => ms_set_sl_grid as extern "C" fn(u64, u32),
        "nvnMultisampleStateSetSampleLocations" => ms_set_sample_locations as extern "C" fn(u64, i32, i32, u64),
        "nvnMultisampleStateGetMultisampleEnable" => ms_get_enable as extern "C" fn(u64) -> u32,
        "nvnMultisampleStateGetSamples" => ms_get_samples as extern "C" fn(u64) -> i32,
        "nvnMultisampleStateGetAlphaToCoverageEnable" => ms_get_a2c as extern "C" fn(u64) -> u32,
        "nvnMultisampleStateGetAlphaToCoverageDither" => ms_get_a2c_dither as extern "C" fn(u64) -> u32,
        "nvnMultisampleStateGetRasterSamples" => ms_get_raster as extern "C" fn(u64) -> i32,
        "nvnMultisampleStateGetCoverageModulationMode" => ms_get_cov_mod as extern "C" fn(u64) -> i32,
        "nvnMultisampleStateGetCoverageToColorEnable" => ms_get_c2c_en as extern "C" fn(u64) -> u32,
        "nvnMultisampleStateGetCoverageToColorOutput" => ms_get_c2c_out as extern "C" fn(u64) -> i32,
        "nvnMultisampleStateGetSampleLocationsEnable" => ms_get_sl_en as extern "C" fn(u64) -> u32,
        "nvnMultisampleStateGetSampleLocationsGridEnable" => ms_get_sl_grid as extern "C" fn(u64) -> u32,
        "nvnPolygonStateSetDefaults" => poly_set_defaults as extern "C" fn(u64),
        "nvnPolygonStateSetCullFace" => poly_set_cull as extern "C" fn(u64, i32),
        "nvnPolygonStateSetFrontFace" => poly_set_front as extern "C" fn(u64, i32),
        "nvnPolygonStateSetPolygonMode" => poly_set_mode as extern "C" fn(u64, i32),
        "nvnPolygonStateSetPolygonOffsetEnables" => poly_set_offset as extern "C" fn(u64, i32),
        "nvnPolygonStateGetCullFace" => poly_get_cull as extern "C" fn(u64) -> i32,
        "nvnPolygonStateGetFrontFace" => poly_get_front as extern "C" fn(u64) -> i32,
        "nvnPolygonStateGetPolygonMode" => poly_get_mode as extern "C" fn(u64) -> i32,
        "nvnPolygonStateGetPolygonOffsetEnables" => poly_get_offset as extern "C" fn(u64) -> i32,
        "nvnDepthStencilStateSetDefaults" => ds_set_defaults as extern "C" fn(u64),
        "nvnDepthStencilStateSetDepthTestEnable" => ds_set_depth_test as extern "C" fn(u64, u32),
        "nvnDepthStencilStateSetDepthWriteEnable" => ds_set_depth_write as extern "C" fn(u64, u32),
        "nvnDepthStencilStateSetDepthFunc" => ds_set_depth_func as extern "C" fn(u64, i32),
        "nvnDepthStencilStateSetStencilTestEnable" => ds_set_stencil_test as extern "C" fn(u64, u32),
        "nvnDepthStencilStateSetStencilFunc" => ds_set_stencil_func as extern "C" fn(u64, i32, i32),
        "nvnDepthStencilStateSetStencilOp" => ds_set_stencil_op as extern "C" fn(u64, i32, i32, i32, i32),
        "nvnDepthStencilStateGetDepthTestEnable" => ds_get_depth_test as extern "C" fn(u64) -> u32,
        "nvnDepthStencilStateGetDepthWriteEnable" => ds_get_depth_write as extern "C" fn(u64) -> u32,
        "nvnDepthStencilStateGetDepthFunc" => ds_get_depth_func as extern "C" fn(u64) -> i32,
        "nvnDepthStencilStateGetStencilTestEnable" => ds_get_stencil_test as extern "C" fn(u64) -> u32,
        "nvnDepthStencilStateGetStencilFunc" => ds_get_stencil_func as extern "C" fn(u64, i32) -> i32,
        "nvnDepthStencilStateGetStencilOp" => ds_get_stencil_op as extern "C" fn(u64, i32, u64, u64, u64),
        "nvnVertexAttribStateSetDefaults" => va_set_defaults as extern "C" fn(u64),
        "nvnVertexAttribStateSetFormat" => va_set_format as extern "C" fn(u64, i32, i64),
        "nvnVertexAttribStateSetStreamIndex" => va_set_stream as extern "C" fn(u64, i32),
        "nvnVertexAttribStateGetFormat" => va_get_format as extern "C" fn(u64, u64, u64),
        "nvnVertexAttribStateGetStreamIndex" => va_get_stream as extern "C" fn(u64) -> i32,
        "nvnVertexStreamStateSetDefaults" => vs_set_defaults as extern "C" fn(u64),
        "nvnVertexStreamStateSetStride" => vs_set_stride as extern "C" fn(u64, i64),
        "nvnVertexStreamStateSetDivisor" => vs_set_divisor as extern "C" fn(u64, i32),
        "nvnVertexStreamStateGetStride" => vs_get_stride as extern "C" fn(u64) -> i64,
        "nvnVertexStreamStateGetDivisor" => vs_get_divisor as extern "C" fn(u64) -> i32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    fn obj() -> u64 {
        testing::alloc(0x40)
    }

    #[test]
    fn blend_defaults_are_one_zero_add() {
        let s = obj();
        blend_set_defaults(s);
        let b = blend(s).expect("blend");
        assert_eq!((b.src_rgb, b.dst_rgb, b.src_alpha, b.dst_alpha), (2, 1, 2, 1));
        assert_eq!((b.eq_rgb, b.eq_alpha, b.advanced_mode), (1, 1, 0));
        assert!(b.premultiplied_src && b.normalized_dst);
    }

    #[test]
    fn blend_fields_do_not_overlap() {
        let s = obj();
        blend_set_defaults(s);
        blend_set_target(s, 7);
        blend_set_func(s, 0x7f, 0x55, 0x2a, 0x7f);
        blend_set_equation(s, 7, 5);
        blend_set_advanced_mode(s, 0x32);
        blend_set_premultiplied(s, 0);
        blend_set_normalized(s, 0);
        blend_set_overlap(s, 3);
        let b = blend(s).expect("blend");
        assert_eq!((b.target, b.src_rgb, b.dst_rgb, b.src_alpha, b.dst_alpha), (7, 0x7f, 0x55, 0x2a, 0x7f));
        assert_eq!((b.eq_rgb, b.eq_alpha, b.advanced_mode, b.overlap), (7, 5, 0x32, 3));
        assert!(!b.premultiplied_src && !b.normalized_dst);
        assert_eq!(blend_get_premultiplied(s), 0);
        blend_set_advanced_mode(s, 0x33);
        assert_eq!(blend_get_advanced_mode(s), 0);
    }

    #[test]
    fn color_blend_enable_ignores_a_bad_index() {
        let s = obj();
        color_set_defaults(s);
        color_set_blend_enable(s, 2, 1);
        color_set_blend_enable(s, 8, 1);
        color_set_blend_enable(s, -1, 1);
        let c = color(s).expect("color");
        assert_eq!(c.blend_enable, 0b100);
        assert_eq!((c.logic_op, c.alpha_func), (3, 8));
        assert_eq!(color_get_blend_enable(s, 2), 1);
        assert_eq!(color_get_blend_enable(s, 9), 0);
        color_set_blend_enable(s, 2, 0);
        assert!(!color(s).expect("color").any_blend());
    }

    #[test]
    fn channel_mask_packs_four_bits_per_target() {
        let s = obj();
        mask_set_defaults(s);
        mask_set(s, 1, 1, 0, 1, 0);
        mask_set(s, 8, 0, 0, 0, 0);
        let m = channel_mask(s).expect("mask");
        assert_eq!(m.channels(1), 0b0101);
        assert_eq!(m.channels(0), 0xf);
        assert_eq!(m.written_targets(), 0xff);
        let out = obj();
        mask_get(s, 1, out, out + 1, out + 2, out + 3);
        let mut b = [0u8; 4];
        Obj(out).read(0, &mut b).expect("read");
        assert_eq!(b, [1, 0, 1, 0]);
        mask_get(s, 1, 0, 0, 0, 0);
    }

    #[test]
    fn depth_stencil_sets_the_faces_the_mask_names() {
        let s = obj();
        ds_set_defaults(s);
        let d = depth_stencil(s).expect("ds");
        assert!(!d.depth_test && d.depth_write && !d.stencil_test);
        assert_eq!((d.depth_func, d.front.func, d.back.zpass), (2, 8, 1));
        ds_set_stencil_func(s, 2, 3);
        ds_set_stencil_op(s, 1, 4, 5, 6);
        let d = depth_stencil(s).expect("ds");
        assert_eq!((d.front.func, d.back.func), (8, 3));
        assert_eq!((d.front.fail, d.front.zfail, d.front.zpass), (4, 5, 6));
        assert_eq!((d.back.fail, d.back.zfail, d.back.zpass), (1, 1, 1));
        ds_set_stencil_func(s, 3, 9);
        assert_eq!(ds_get_stencil_func(s, 1), 9);
        assert_eq!(ds_get_stencil_func(s, 2), 9);
    }

    #[test]
    fn multisample_defaults_enable_and_keep_each_byte() {
        let s = obj();
        ms_set_defaults(s);
        assert_eq!(ms_get_enable(s), 1);
        ms_set_samples(s, 4);
        ms_set_a2c(s, 1);
        ms_set_sl_grid(s, 1);
        let m = multisample(s).expect("ms");
        assert!(m.enable && m.alpha_to_coverage && m.sample_locations_grid);
        assert_eq!((m.samples, m.raster_samples), (4, 0));
    }

    #[test]
    fn polygon_defaults_and_fields() {
        let s = obj();
        poly_set_defaults(s);
        let p = polygon(s).expect("polygon");
        assert_eq!((p.cull_face, p.front_face, p.mode, p.offset_enables), (0, 1, 2, 0));
        poly_set_cull(s, 2);
        poly_set_front(s, 0);
        poly_set_mode(s, 1);
        poly_set_offset(s, 7);
        let p = polygon(s).expect("polygon");
        assert_eq!((p.cull_face, p.front_face, p.mode, p.offset_enables), (2, 0, 1, 7));
    }

    #[test]
    fn vertex_attrib_and_stream_round_trip() {
        let s = obj();
        va_set_defaults(s);
        va_set_format(s, 0x2a, 0x7_ffff);
        va_set_stream(s, 31);
        let a = vertex_attrib(s).expect("attrib");
        assert_eq!((a.format, a.offset, a.stream), (0x2a, 0x7_ffff, 31));
        let out = obj();
        va_get_format(s, out, out + 8);
        assert_eq!(Obj(out).i32(0).expect("format"), 0x2a);
        assert_eq!(Obj(out).i64(8).expect("offset"), 0x7_ffff);
        va_get_format(s, 0, 0);
        vs_set_stride(s + 0x20, 0x1_0000_0010);
        vs_set_divisor(s + 0x20, 3);
        assert_eq!(vs_get_stride(s + 0x20), 0x10);
        let v = vertex_stream(s + 0x20).expect("stream");
        assert_eq!((v.stride, v.divisor), (0x10, 3));
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: Vec<&str> = procs().iter().map(|(n, _)| *n).collect();
        let set: std::collections::HashSet<&str> = got.iter().copied().collect();
        assert_eq!(set.len(), got.len(), "a name is exported twice");
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "state")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(set, want);
    }
}
