//! The device group: `nvnDevice*`, the device builder, the device queries and the handle getters.
//!
//! The `NVNdevice` is 0x3000 bytes that the game owns. The host uses the first bytes:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | magic `DEV\0` while initialised |
//! | 4 | flags of the builder |
//! | 8 | window origin mode (`NVNwindowOriginMode`, 0 is lower left and the default) |
//! | 12 | depth mode (`NVNdepthMode`, 0 is near plane at -w and the default) |
//! | 16 | the debug callback, 0 when the game switched it off |
//! | 24 | the data pointer of the debug callback |
//!
//! The device builder is 0x40 bytes with the flags at offset 0.
//!
//! The texture and sampler handles are numbers. A texture handle is `texture id | sampler id << 20`, as on the GPU.
//! The host calls the debug callback of a game never. A game that installs one gets none of its messages.
//!
//! `nvnDeviceGetInteger` answers from a table of the values that the Switch driver reports for the NVN 55 API.
//! The table is the one `nxnative` uses. Items 0 and 1 are the API version of the [`Settings`] and items 0x2E and
//! 0x30 are the GPU code version of the settings. Item 0x5F (4096) is the one value that `nxnative` marks as unverified.
//! The Switch 2 table of `nxnative` is not ported, so a game of the Switch 2 sees the Switch 1 values.

use super::abi::{AbiResult, Obj, arg_bool, guard, log_once, magic, ret_bool, size, table};
use super::ctx::{Settings, gpu};
use super::{lookup, name_at};

/// The magic of an initialised device.
const DEV: u32 = magic(b'D', b'E', b'V', 0);
const OFF_FLAGS: u64 = 4;
const OFF_ORIGIN: u64 = 8;
const OFF_DEPTH: u64 = 12;
const OFF_DBG_CB: u64 = 16;
const OFF_DBG_DATA: u64 = 24;
/// `NVN_WINDOW_ORIGIN_MODE_LOWER_LEFT`.
const ORIGIN_LOWER_LEFT: i32 = 0;
/// One tick of the GPU counter lasts 625/384 nanoseconds (614.4 MHz).
const TICK_NUM: u128 = 625;
const TICK_DEN: u128 = 384;
/// `NVN_DEVICE_INFO_*` items from this number on are not in the table.
const INFO_TABLE_LEN: usize = 0x60;
/// The one item above the table that nxnative answers.
const INFO_EXTRA: i32 = 0x67;

/// The values of `NVNdeviceInfo` items 0 to 0x5F. Items 0 and 1 are replaced by the settings.
/// Item 0x5F is the one value that nxnative does not mark as checked against a device.
const INFO: [u32; INFO_TABLE_LEN] = [
    55, 15, 14, 65536, 256, 8, 16, 4, 16, 32, 16, 4, 4, 4, 16, 32, //
    32, 256, 256, 4, 8, 512, 256, 2, 256, 4096, 131072, 16, 1536, 1024, 64, 1536, //
    65535, 65535, 65535, 8, 1048576, 4096, 16, 4096, 1, 1, 1, 262144, 15, 2048, 1, 1, //
    14, 5, 1, 8, 8, 4, 32, 128, 32, 128, 65536, 1, 4, 16384, 134217728, 2048, //
    16384, 16384, 1, 1, 1, 1, 5, 1, 1, 1, 1, 1, 1, 16, 1024, 32, //
    4096, 65536, 65536, 256, 16384, 262144, 4096, 1, 4, 2, 0, 4096, 16384, 4096, 128, 4096,
];

extern "C" fn device_builder_set_defaults(b: u64) {
    guard("nvnDeviceBuilderSetDefaults", || {
        Obj(b).zero(size::DEVICE_BUILDER)
    });
}

extern "C" fn device_builder_set_flags(b: u64, flags: i32) {
    guard("nvnDeviceBuilderSetFlags", || Obj(b).set_i32(0, flags));
}

extern "C" fn device_initialize(d: u64, b: u64) -> u32 {
    guard("nvnDeviceInitialize", || {
        let flags = Obj(b).i32(0)?;
        let o = Obj(d);
        o.zero(size::DEVICE)?;
        o.set_u32(0, DEV)?;
        o.set_i32(OFF_FLAGS, flags)?;
        o.set_i32(OFF_ORIGIN, ORIGIN_LOWER_LEFT)?;
        gpu("nvnDeviceInitialize")
            .backend()
            .device_modes(true, false);
        Ok(1)
    })
}

extern "C" fn device_finalize(d: u64) {
    guard("nvnDeviceFinalize", || Obj(d).set_u32(0, 0));
}

/// Whether the window origin of the device at `d` is the lower left. A null device counts as lower left, the default.
pub(super) fn origin_lower_left(d: u64) -> AbiResult<bool> {
    if d == 0 {
        return Ok(true);
    }
    Ok(Obj(d).i32(OFF_ORIGIN)? == ORIGIN_LOWER_LEFT)
}

/// The origin and the depth mode of a device, as the backend takes them.
fn notify_modes(name: &'static str, d: Obj) -> AbiResult<()> {
    let lower_left = d.i32(OFF_ORIGIN)? == ORIGIN_LOWER_LEFT;
    let zero_to_one = d.i32(OFF_DEPTH)? != 0;
    gpu(name).backend().device_modes(lower_left, zero_to_one);
    Ok(())
}

extern "C" fn device_set_window_origin_mode(d: u64, mode: i32) {
    guard("nvnDeviceSetWindowOriginMode", || {
        Obj(d).set_i32(OFF_ORIGIN, mode)?;
        notify_modes("nvnDeviceSetWindowOriginMode", Obj(d))
    });
}

extern "C" fn device_get_window_origin_mode(d: u64) -> i32 {
    guard("nvnDeviceGetWindowOriginMode", || Obj(d).i32(OFF_ORIGIN))
}

extern "C" fn device_set_depth_mode(d: u64, mode: i32) {
    guard("nvnDeviceSetDepthMode", || {
        Obj(d).set_i32(OFF_DEPTH, mode)?;
        notify_modes("nvnDeviceSetDepthMode", Obj(d))
    });
}

extern "C" fn device_get_depth_mode(d: u64) -> i32 {
    guard("nvnDeviceGetDepthMode", || Obj(d).i32(OFF_DEPTH))
}

extern "C" fn device_install_debug_callback(d: u64, cb: u64, data: u64, enable: u32) {
    guard("nvnDeviceInstallDebugCallback", || {
        let o = Obj(d);
        o.set_u64(OFF_DBG_CB, if arg_bool(enable) { cb } else { 0 })?;
        o.set_u64(OFF_DBG_DATA, data)
    });
}

extern "C" fn device_finalize_command_handle(_d: u64, _handle: u64) {}

extern "C" fn device_register_fast_clear_color(_d: u64, _color: u64, _format: i32) -> u32 {
    ret_bool(true)
}

extern "C" fn device_register_fast_clear_depth(_d: u64, _depth: f32) -> u32 {
    ret_bool(true)
}

extern "C" fn device_set_intermediate_shader_cache(_d: u64, _n: i32) {}

extern "C" fn set_external_debugger_enabled(_enable: u32) {}

/// The value of an `NVNdeviceInfo` item. None for an item that nxnative does not answer.
fn info_value(item: i32, s: &Settings) -> Option<u32> {
    match item {
        0 => Some(s.api_major),
        1 => Some(s.api_minor),
        0x2E => Some(s.gpu_code.0),
        0x30 => Some(s.gpu_code.1),
        INFO_EXTRA => Some(256),
        i if (0..INFO_TABLE_LEN as i32).contains(&i) => Some(INFO[i as usize]),
        _ => None,
    }
}

extern "C" fn device_get_integer(_d: u64, item: i32, out: u64) {
    guard("nvnDeviceGetInteger", || {
        match info_value(item, gpu("nvnDeviceGetInteger").settings()) {
            Some(v) => Obj(out).set_u32(0, v),
            None => {
                eprintln!(
                    "[RED] NVN nvnDeviceGetInteger: item {item:#x} is not known, the host answers 0"
                );
                Obj(out).set_u32(0, 0)
            }
        }
    });
}

/// Converts GPU counter ticks to nanoseconds, rounded down.
fn ticks_to_ns(ticks: u64) -> u64 {
    (u128::from(ticks) * TICK_NUM / TICK_DEN) as u64
}

extern "C" fn device_get_timestamp_in_nanoseconds(_d: u64, counter: u64) -> u64 {
    guard("nvnDeviceGetTimestampInNanoseconds", || {
        Ok(ticks_to_ns(Obj(counter).u64(8)?))
    })
}

extern "C" fn device_get_current_timestamp_in_nanoseconds(_d: u64) -> u64 {
    guard("nvnDeviceGetCurrentTimestampInNanoseconds", || {
        Ok((gpu("nvnDeviceGetCurrentTimestampInNanoseconds")
            .settings()
            .clock_ns)())
    })
}

extern "C" fn device_get_texture_handle(_d: u64, tex: i32, sampler: i32) -> u64 {
    u64::from(tex as u32) | u64::from(sampler as u32) << 20
}

extern "C" fn device_get_texel_fetch_handle(_d: u64, tex: i32) -> u64 {
    u64::from(tex as u32)
}

extern "C" fn device_get_image_handle(_d: u64, id: i32) -> u64 {
    id as u32 as u64
}

extern "C" fn device_get_separate_texture_handle(_d: u64, id: i32) -> u64 {
    id as u32 as u64
}

extern "C" fn device_get_separate_sampler_handle(_d: u64, id: i32) -> u64 {
    (id as u32 as u64) << 20
}

/// `nvnDeviceGetProcAddress(const NVNdevice *, const char *name)`. The device may be NULL, as in `Smash`.
extern "C" fn device_get_proc_address(_d: u64, name: u64) -> u64 {
    guard("nvnDeviceGetProcAddress", || {
        let addr = lookup(&name_at(name)?);
        if addr == 0 {
            log_once(
                "nvnDeviceGetProcAddress",
                "nvnDeviceGetProcAddress gave NULL for a name that no group exports",
            );
        }
        Ok(addr as u64)
    })
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnDeviceBuilderSetDefaults" => device_builder_set_defaults as extern "C" fn(u64),
        "nvnDeviceBuilderSetFlags" => device_builder_set_flags as extern "C" fn(u64, i32),
        "nvnDeviceInitialize" => device_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnDeviceFinalize" => device_finalize as extern "C" fn(u64),
        "nvnDeviceFinalizeCommandHandle" => device_finalize_command_handle as extern "C" fn(u64, u64),
        "nvnDeviceGetProcAddress" => device_get_proc_address as extern "C" fn(u64, u64) -> u64,
        "nvnDeviceGetInteger" => device_get_integer as extern "C" fn(u64, i32, u64),
        "nvnDeviceGetCurrentTimestampInNanoseconds" => device_get_current_timestamp_in_nanoseconds as extern "C" fn(u64) -> u64,
        "nvnDeviceGetTimestampInNanoseconds" => device_get_timestamp_in_nanoseconds as extern "C" fn(u64, u64) -> u64,
        "nvnDeviceGetTextureHandle" => device_get_texture_handle as extern "C" fn(u64, i32, i32) -> u64,
        "nvnDeviceGetTexelFetchHandle" => device_get_texel_fetch_handle as extern "C" fn(u64, i32) -> u64,
        "nvnDeviceGetImageHandle" => device_get_image_handle as extern "C" fn(u64, i32) -> u64,
        "nvnDeviceGetSeparateTextureHandle" => device_get_separate_texture_handle as extern "C" fn(u64, i32) -> u64,
        "nvnDeviceGetSeparateSamplerHandle" => device_get_separate_sampler_handle as extern "C" fn(u64, i32) -> u64,
        "nvnDeviceInstallDebugCallback" => device_install_debug_callback as extern "C" fn(u64, u64, u64, u32),
        "nvnDeviceSetWindowOriginMode" => device_set_window_origin_mode as extern "C" fn(u64, i32),
        "nvnDeviceGetWindowOriginMode" => device_get_window_origin_mode as extern "C" fn(u64) -> i32,
        "nvnDeviceSetDepthMode" => device_set_depth_mode as extern "C" fn(u64, i32),
        "nvnDeviceGetDepthMode" => device_get_depth_mode as extern "C" fn(u64) -> i32,
        "nvnDeviceRegisterFastClearColor" => device_register_fast_clear_color as extern "C" fn(u64, u64, i32) -> u32,
        "nvnDeviceRegisterFastClearDepth" => device_register_fast_clear_depth as extern "C" fn(u64, f32) -> u32,
        "nvnDeviceSetIntermediateShaderCache" => device_set_intermediate_shader_cache as extern "C" fn(u64, i32),
        "nvnSetExternalDebuggerEnabledNVX" => set_external_debugger_enabled as extern "C" fn(u32),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_the_values_of_nxnative_for_the_first_items() {
        assert_eq!(INFO.len(), 0x60);
        assert_eq!(INFO[3], 65536);
        assert_eq!(INFO[0x1A], 131072);
        assert_eq!(INFO[0x24], 1_048_576);
        assert_eq!(INFO[0x3E], 134_217_728);
        assert_eq!(INFO[0x5A], 0);
    }

    #[test]
    fn items_0_1_and_the_gpu_code_come_from_the_settings_and_unknown_items_have_no_value() {
        let s = Settings {
            api_major: 60,
            api_minor: 1,
            gpu_code: (1, 17),
            ..Settings::default()
        };
        assert_eq!(info_value(0, &s), Some(60));
        assert_eq!(info_value(1, &s), Some(1));
        assert_eq!(info_value(0x2E, &s), Some(1));
        assert_eq!(info_value(0x30, &s), Some(17));
        assert_eq!(info_value(2, &s), Some(14));
        assert_eq!(info_value(0x67, &s), Some(256));
        assert_eq!(info_value(0x60, &s), None);
        assert_eq!(info_value(-1, &s), None);
    }

    #[test]
    fn counter_ticks_become_nanoseconds_at_614_4_mhz() {
        assert_eq!(ticks_to_ns(0), 0);
        assert_eq!(ticks_to_ns(384), 625);
        assert_eq!(ticks_to_ns(614_400_000), 1_000_000_000);
        assert_eq!(
            ticks_to_ns(u64::MAX),
            (u128::from(u64::MAX) * 625 / 384) as u64
        );
    }

    #[test]
    fn handles_pack_the_texture_in_the_low_20_bits_and_the_sampler_above() {
        assert_eq!(device_get_texture_handle(0, 5, 3), 5 | 3 << 20);
        assert_eq!(device_get_texel_fetch_handle(0, 7), 7);
        assert_eq!(device_get_separate_sampler_handle(0, 2), 2 << 20);
        assert_eq!(device_get_separate_texture_handle(0, 9), 9);
        assert_eq!(device_get_image_handle(0, 4), 4);
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: std::collections::HashSet<&str> = procs().iter().map(|(n, _)| *n).collect();
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "device")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(got, want);
    }
}
