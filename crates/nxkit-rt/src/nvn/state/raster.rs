//! The multisample and polygon state objects.

use super::*;

// Multisample state.

pub(super) extern "C" fn ms_set_defaults(s: u64) {
    guard("nvnMultisampleStateSetDefaults", || {
        Obj(s).zero(MS_SIZE)?;
        set_byte(s, MS_ENABLE, 1)
    });
}

ms_set!(
    ms_set_enable,
    "nvnMultisampleStateSetMultisampleEnable",
    MS_ENABLE,
    flag
);
ms_set!(
    ms_set_samples,
    "nvnMultisampleStateSetSamples",
    MS_SAMPLES,
    int
);
ms_set!(
    ms_set_a2c,
    "nvnMultisampleStateSetAlphaToCoverageEnable",
    MS_A2C,
    flag
);
ms_set!(
    ms_set_a2c_dither,
    "nvnMultisampleStateSetAlphaToCoverageDither",
    MS_A2C_DITHER,
    flag
);
ms_set!(
    ms_set_raster,
    "nvnMultisampleStateSetRasterSamples",
    MS_RASTER,
    int
);
ms_set!(
    ms_set_cov_mod,
    "nvnMultisampleStateSetCoverageModulationMode",
    MS_COV_MOD,
    int
);
ms_set!(
    ms_set_c2c_en,
    "nvnMultisampleStateSetCoverageToColorEnable",
    MS_C2C_EN,
    flag
);
ms_set!(
    ms_set_c2c_out,
    "nvnMultisampleStateSetCoverageToColorOutput",
    MS_C2C_OUT,
    int
);
ms_set!(
    ms_set_sl_en,
    "nvnMultisampleStateSetSampleLocationsEnable",
    MS_SL_EN,
    flag
);
ms_set!(
    ms_set_sl_grid,
    "nvnMultisampleStateSetSampleLocationsGridEnable",
    MS_SL_GRID,
    flag
);
ms_get!(
    ms_get_enable,
    "nvnMultisampleStateGetMultisampleEnable",
    MS_ENABLE,
    flag
);
ms_get!(
    ms_get_samples,
    "nvnMultisampleStateGetSamples",
    MS_SAMPLES,
    int
);
ms_get!(
    ms_get_a2c,
    "nvnMultisampleStateGetAlphaToCoverageEnable",
    MS_A2C,
    flag
);
ms_get!(
    ms_get_a2c_dither,
    "nvnMultisampleStateGetAlphaToCoverageDither",
    MS_A2C_DITHER,
    flag
);
ms_get!(
    ms_get_raster,
    "nvnMultisampleStateGetRasterSamples",
    MS_RASTER,
    int
);
ms_get!(
    ms_get_cov_mod,
    "nvnMultisampleStateGetCoverageModulationMode",
    MS_COV_MOD,
    int
);
ms_get!(
    ms_get_c2c_en,
    "nvnMultisampleStateGetCoverageToColorEnable",
    MS_C2C_EN,
    flag
);
ms_get!(
    ms_get_c2c_out,
    "nvnMultisampleStateGetCoverageToColorOutput",
    MS_C2C_OUT,
    int
);
ms_get!(
    ms_get_sl_en,
    "nvnMultisampleStateGetSampleLocationsEnable",
    MS_SL_EN,
    flag
);
ms_get!(
    ms_get_sl_grid,
    "nvnMultisampleStateGetSampleLocationsGridEnable",
    MS_SL_GRID,
    flag
);

/// The programmable sample positions are not kept. The call is logged once.
pub(super) extern "C" fn ms_set_sample_locations(_s: u64, _start: i32, _n: i32, _v: u64) {
    log_once(
        "nvnMultisampleStateSetSampleLocations",
        format_args!("nvnMultisampleStateSetSampleLocations: the sample positions are ignored"),
    );
}

// Polygon state.

pub(super) extern "C" fn poly_set_defaults(s: u64) {
    guard("nvnPolygonStateSetDefaults", || {
        let w = pack(
            pack(0, PO_FRONT, FRONT_FACE_CCW),
            PO_MODE,
            POLYGON_MODE_FILL,
        );
        store(s, POLY, w)
    });
}

set_field!(poly_set_cull, "nvnPolygonStateSetCullFace", POLY, PO_CULL);
set_field!(
    poly_set_front,
    "nvnPolygonStateSetFrontFace",
    POLY,
    PO_FRONT
);
set_field!(
    poly_set_mode,
    "nvnPolygonStateSetPolygonMode",
    POLY,
    PO_MODE
);
set_field!(
    poly_set_offset,
    "nvnPolygonStateSetPolygonOffsetEnables",
    POLY,
    PO_OFFSET
);
get_field!(poly_get_cull, "nvnPolygonStateGetCullFace", POLY, PO_CULL);
get_field!(
    poly_get_front,
    "nvnPolygonStateGetFrontFace",
    POLY,
    PO_FRONT
);
get_field!(
    poly_get_mode,
    "nvnPolygonStateGetPolygonMode",
    POLY,
    PO_MODE
);
get_field!(
    poly_get_offset,
    "nvnPolygonStateGetPolygonOffsetEnables",
    POLY,
    PO_OFFSET
);
