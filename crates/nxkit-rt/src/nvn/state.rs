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
//! Defaults and the bounds of the indexes were checked on 2026-11-07 against `dm/gfx/nvn/hle_nvn.c`.
//! The NVN enum values are those of `dm/gfx/nvn/include/nvn.h`.

use super::abi::{AbiResult, Obj, arg_bool, guard, log_once, ret_bool, table};

#[cfg(test)]
mod read;

// The readers are for the command buffer group; only the tests use them until it does.
#[cfg(test)]
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
        pub(super) extern "C" fn $fname(s: u64, v: i32) {
            guard($name, || put(s, $bytes, $field, v as u32))
        }
    };
}

/// A setter of one bit-field that holds an NVNboolean.
macro_rules! set_flag {
    ($fname:ident, $name:literal, $bytes:expr, $field:expr) => {
        pub(super) extern "C" fn $fname(s: u64, v: u32) {
            guard($name, || put(s, $bytes, $field, u32::from(arg_bool(v))))
        }
    };
}

/// A getter of one bit-field that returns an enum or an integer.
macro_rules! get_field {
    ($fname:ident, $name:literal, $bytes:expr, $field:expr) => {
        pub(super) extern "C" fn $fname(s: u64) -> i32 {
            guard($name, || Ok(get(s, $bytes, $field)? as i32))
        }
    };
}

/// A getter of one bit-field that returns an NVNboolean.
macro_rules! get_flag {
    ($fname:ident, $name:literal, $bytes:expr, $field:expr) => {
        pub(super) extern "C" fn $fname(s: u64) -> u32 {
            guard($name, || Ok(ret_bool(get(s, $bytes, $field)? != 0)))
        }
    };
}

/// A setter of one byte of the multisample state.
macro_rules! ms_set {
    ($fname:ident, $name:literal, $off:expr, int) => {
        pub(super) extern "C" fn $fname(s: u64, v: i32) {
            guard($name, || set_byte(s, $off, v as u8))
        }
    };
    ($fname:ident, $name:literal, $off:expr, flag) => {
        pub(super) extern "C" fn $fname(s: u64, v: u32) {
            guard($name, || set_byte(s, $off, u8::from(arg_bool(v))))
        }
    };
}

/// A getter of one byte of the multisample state.
macro_rules! ms_get {
    ($fname:ident, $name:literal, $off:expr, int) => {
        pub(super) extern "C" fn $fname(s: u64) -> i32 {
            guard($name, || Ok(i32::from(byte(s, $off)?)))
        }
    };
    ($fname:ident, $name:literal, $off:expr, flag) => {
        pub(super) extern "C" fn $fname(s: u64) -> u32 {
            guard($name, || Ok(u32::from(byte(s, $off)?)))
        }
    };
}

mod blend;
mod depth;
mod raster;
#[cfg(test)]
mod tests;
mod vertex;

use blend::*;
use depth::*;
use raster::*;
use vertex::*;

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
