//! The device group: `nvnDevice*`, the device builder, the device queries and the handle getters.
//!
//! The `NVNdevice` is 0x3000 bytes that the game owns. The host uses and writes the first 0x20 bytes only:
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
//! The device builder is 0x40 bytes in the headers. The host uses the flags at offset 0 and writes 4 bytes.
//!
//! The texture and sampler handles are numbers. A texture handle is `texture id | sampler id << 20`, as on the GPU.
//! The host calls the debug callback of a game never. A game that installs one gets none of its messages.
//!
//! `nvnDeviceGetInteger` answers from a table of the values that the Switch driver reports for the NVN 55 API.
//! The table is the one the engine uses. Items 0 and 1 are the API version of the [`Settings`] and items 0x2E and
//! 0x30 are the GPU code version of the settings. Item 0x5F (4096) is the one value that the engine marks as unverified.
//!
//! A game of the Switch 2 gets the table of NVN2 (API 231) instead, up to item 0xA9. NVN2 is compiled into the game's
//! own `sdk` module and answers most items from constants in one `switch`. The host finds that `switch` once at load
//! ([`scan_sdk_driver`]) and answers an item from the game's own constant where the `switch` has one: the API minor
//! and the GPU code versions move with the SDK. The table has the newest values seen as the fallback. NVN2 reads a
//! few items from the description of the GPU at run time. These keep the Switch value below 0x60 and are 0 above it.

use std::sync::OnceLock;

use nxkit_abi::guest;
use nxkit_abi::platform::{self, Platform};

use super::abi::{AbiResult, Obj, arg_bool, guard, log_once, magic, ret_bool, table, used};
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
/// The one item above the table that the engine answers.
const INFO_EXTRA: i32 = 0x67;

/// The values of `NVNdeviceInfo` items 0 to 0x5F. Items 0 and 1 are replaced by the settings.
/// Item 0x5F is the one value that the engine does not mark as checked against a device.
const INFO: [u32; INFO_TABLE_LEN] = [
    55, 15, 14, 65536, 256, 8, 16, 4, 16, 32, 16, 4, 4, 4, 16, 32, //
    32, 256, 256, 4, 8, 512, 256, 2, 256, 4096, 131072, 16, 1536, 1024, 64, 1536, //
    65535, 65535, 65535, 8, 1048576, 4096, 16, 4096, 1, 1, 1, 262144, 15, 2048, 1, 1, //
    14, 5, 1, 8, 8, 4, 32, 128, 32, 128, 65536, 1, 4, 16384, 134217728, 2048, //
    16384, 16384, 1, 1, 1, 1, 5, 1, 1, 1, 1, 1, 1, 16, 1024, 32, //
    4096, 65536, 65536, 256, 16384, 262144, 4096, 1, 4, 2, 0, 4096, 16384, 4096, 128, 4096,
];

/// An item of the NVN2 table that the driver reads from the GPU at run time. The binary does not tell its value.
const RT: i32 = -1;
/// `NVN_DEVICE_INFO_*` items from this number on are not in the NVN2 table.
const INFO2_LEN: usize = 0xAA;
/// The API minor of NVN2 when the game's `sdk` gives none: the newest seen, SDK 21.4.4.
pub const NVN2_API_MINOR: u32 = 39;
/// The API major of NVN2.
pub const NVN2_API_MAJOR: u32 = 231;

/// The values of the NVN2 items 0 to 0xA9, from the engine (`g_devinfo2`): the constants of the driver in the `sdk`
/// of SDK 21.4.4, read statically. Items 0 and 1 are the API version 231.39, items 0x2E to 0x31 the GPU code version
/// 201.0 to 201.20 (0xC9 is the version of the control sections of its shaders). Item 6 is 32 on NVN2. The host keeps
/// 16 vertex bindings, which all its tables hold. [`RT`] marks an item that the driver reads from the GPU.
#[rustfmt::skip]
const INFO2: [i32; INFO2_LEN] = [
    231, 39, 14, 65536, 256, 8, 16, 4, 16, 32, 16, 4, 4, 4, 32, 32, // 0x00
    32, 256, 256, 4, 8, 512, 256, RT, RT, 4096, 16384, 16, 1024, 1024, 64, 1024, // 0x10
    2147483647, 65535, 65535, 8, 1048576, 4096, 16, 4096, 1, 1, 1, RT, 16, RT, 201, 201, // 0x20
    20, 0, 1, 8, 8, 4, 32, RT, RT, 128, RT, 1, 4, RT, RT, RT, // 0x30
    RT, RT, 1, 1, 1, 1, 5, 1, 1, 1, 1, 1, 1, 16, 2048, 32, // 0x40
    4096, 65536, 65536, 256, 16384, 262144, 4096, 1, 4, 2, 0, 4096, 16384, 4096, 128, 32, // 0x50
    65535, 1, RT, 256, 16, 65535, 65535, 256, 1, 16, 16, 16, 1, 1, 1, 256, // 0x60
    0, 16, 8, 16, 32, 31, 536870912, 16777216, 16777216, 1073741824, 32, 1, 64, RT, 1, 2147483647, // 0x70
    32, 32, 4194303, 512, 256, 32, 16383, RT, 8, 0, 0, 4, 256, 16, 0, 1, // 0x80
    31, 0, 64, 1, 58368, 24576, 0, 256, 262144, RT, RT, 0, 201, 14, 201, 20, // 0x90
    163840, 8, 4, 256, 256, 4, 65536, 201, 5, 1, // 0xA0
];

/// Where the game's NVN2 driver keeps its `nvnDeviceGetInteger` constants: the `switch` over the items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DriverSwitch {
    /// The guest address of the byte table: one byte per item, the case as a count of instructions from `cases`.
    table: u64,
    /// The guest address of the first case.
    cases: u64,
    /// The last item the `switch` has.
    last: u32,
}

static DRIVER: OnceLock<Option<DriverSwitch>> = OnceLock::new();

/// The signed 21-bit immediate of `adr` and `adrp`.
fn adr_imm(w: u32) -> i64 {
    let v = i64::from(((w >> 3) & 0x1F_FFFC) | ((w >> 29) & 3));
    if v & 0x10_0000 != 0 { v - 0x20_0000 } else { v }
}

/// Finds the `switch` of NVN2's `nvnDeviceGetInteger` in the code of the `sdk` module. `text` is the code with the
/// guest address of its first byte, `ro` the guest range of the read-only data, the data and the bss.
///
/// The driver loads the API major first and then jumps through a byte table: `mov w0, #231`, `adr` (the cases),
/// `adrp` + `add` (the table), `ldrb`, `add .., lsl #2` and `br`, with `cmp wN, #<last item>` before it. All of them
/// lie within 8 instructions of the `mov`. The table must lie in `ro`.
fn find_driver_switch(text: &[u8], text_addr: u64, ro: (u64, u64)) -> Option<DriverSwitch> {
    let t: Vec<u32> = text
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    let at = |q: usize| text_addr + q as u64 * 4;
    let n = t.len();
    let mut p = 8;
    while p + 9 < n {
        if t[p] != 0x5280_1CE0 {
            p += 1;
            continue;
        }
        let (mut table, mut cases, mut lim, mut ldrb, mut br) = (None, None, 0u32, false, false);
        for q in p - 8..=p + 8 {
            let w = t[q];
            if w & 0xFFC0_001F == 0x7100_001F {
                lim = (w >> 10) & 0xFFF; // cmp wN, #imm
            } else if w & 0x9F00_0000 == 0x9000_0000 && t[q + 1] & 0xFFC0_0000 == 0x9100_0000 {
                // adrp + add
                let page = (at(q) & !0xFFF).wrapping_add_signed(adr_imm(w) * 4096);
                table = Some(page + u64::from((t[q + 1] >> 10) & 0xFFF));
            } else if w & 0x9F00_0000 == 0x1000_0000 {
                cases = Some(at(q).wrapping_add_signed(adr_imm(w))); // adr
            } else if w & 0xFFE0_FC00 == 0x3860_6800 {
                ldrb = true; // ldrb wK, [xT, xI]
            } else if w & 0xFFFF_FC1F == 0xD61F_0000 {
                br = true;
            }
        }
        p += 1;
        let (Some(table), Some(cases)) = (table, cases) else {
            continue;
        };
        if lim <= 1 || !ldrb || !br || table < ro.0 || table + u64::from(lim) >= ro.1 {
            continue;
        }
        return Some(DriverSwitch {
            table,
            cases,
            last: lim,
        });
    }
    None
}

/// Looks for the NVN2 driver in the `sdk` module of a game of the Switch 2, once. The host calls it after the load,
/// with the guest ranges of the code and of the read-only data up to the bss. Later calls do nothing.
pub fn scan_sdk_driver(text: (u64, u64), ro: (u64, u64)) {
    DRIVER.get_or_init(|| {
        let found = guest::with_slice(text.0, text.1, |b| find_driver_switch(b, text.0, ro)).ok().flatten();
        if found.is_none() {
            eprintln!(
                "[RED] NVN sdk: NVN2's nvnDeviceGetInteger switch not found; the device answers the newest versions known"
            );
        }
        found
    });
}

/// The constant that the game's NVN2 driver answers for an item, if its `switch` has a constant case for it: `mov
/// w0, #imm16` and `b`. None for a case that reads something else, and before or without [`scan_sdk_driver`].
pub fn sdk_item(item: u32) -> Option<u32> {
    let sw = (*DRIVER.get()?)?;
    if item > sw.last {
        return None;
    }
    let case = sw.cases + u64::from(guest::read_u8(sw.table + u64::from(item)).ok()?) * 4;
    let (c0, c1) = (guest::read_u32(case).ok()?, guest::read_u32(case + 4).ok()?);
    if c0 & 0xFFE0_001F != 0x5280_0000 || c1 & 0xFC00_0000 != 0x1400_0000 {
        return None;
    }
    Some((c0 >> 5) & 0xFFFF)
}

extern "C" fn device_builder_set_defaults(b: u64) {
    guard("nvnDeviceBuilderSetDefaults", || {
        Obj(b).zero(used::DEVICE_BUILDER)
    });
}

extern "C" fn device_builder_set_flags(b: u64, flags: i32) {
    guard("nvnDeviceBuilderSetFlags", || Obj(b).set_i32(0, flags));
}

extern "C" fn device_initialize(d: u64, b: u64) -> u32 {
    guard("nvnDeviceInitialize", || {
        let flags = Obj(b).i32(0)?;
        let o = Obj(d);
        o.zero(used::DEVICE)?;
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
    if crate::diag_on() {
        eprintln!(
            "DIAG depth {name}: origin lower left {lower_left}, depth zero to one {zero_to_one}"
        );
    }
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

/// The answer to an `NVNdeviceInfo` item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Info {
    /// The value.
    Value(u32),
    /// An NVN2 item that the driver reads from the GPU at run time. The host answers 0.
    Runtime,
    /// An item that the engine does not answer. The host answers 0.
    Unknown,
}

/// The value of an `NVNdeviceInfo` item. `sdk` gives the constant of the game's own NVN2 driver for an item.
fn info_value(item: i32, s: &Settings, p: Platform, sdk: impl Fn(u32) -> Option<u32>) -> Info {
    let Ok(i) = usize::try_from(item) else {
        return Info::Unknown;
    };
    match (p, item) {
        (_, 0) => Info::Value(s.api_major),
        (_, 1) => Info::Value(s.api_minor),
        // The Switch 2: the game's constant, then the table. A run-time item below 0x60 keeps the Switch value.
        (Platform::Ounce, _) if i < INFO2_LEN && (INFO2[i] != RT || i >= INFO_TABLE_LEN) => {
            match INFO2[i] {
                RT => Info::Runtime,
                v => Info::Value(sdk(i as u32).unwrap_or(v as u32)),
            }
        }
        (_, 0x2E) => Info::Value(s.gpu_code.0),
        (_, 0x30) => Info::Value(s.gpu_code.1),
        (Platform::Nx, INFO_EXTRA) => Info::Value(256),
        _ if i < INFO_TABLE_LEN => Info::Value(INFO[i]),
        _ => Info::Unknown,
    }
}

extern "C" fn device_get_integer(_d: u64, item: i32, out: u64) {
    guard("nvnDeviceGetInteger", || {
        let s = gpu("nvnDeviceGetInteger").settings();
        match info_value(item, s, platform::get(), sdk_item) {
            Info::Value(v) => Obj(out).set_u32(0, v),
            Info::Runtime => {
                eprintln!(
                    "[RED] NVN nvnDeviceGetInteger: item {item:#x}: NVN2 reads it from the GPU at run time, not known here; the host answers 0"
                );
                Obj(out).set_u32(0, 0)
            }
            Info::Unknown => {
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
    fn the_table_has_the_expected_values_for_the_first_items() {
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
        let v = |i| info_value(i, &s, Platform::Nx, |_| Some(7));
        assert_eq!(v(0), Info::Value(60));
        assert_eq!(v(1), Info::Value(1));
        assert_eq!(v(0x2E), Info::Value(1));
        assert_eq!(v(0x30), Info::Value(17));
        assert_eq!(v(2), Info::Value(14));
        assert_eq!(v(0x67), Info::Value(256));
        assert_eq!(v(0x60), Info::Unknown);
        assert_eq!(v(-1), Info::Unknown);
    }

    #[test]
    fn the_switch_2_answers_from_the_sdk_then_from_its_table() {
        let s = Settings {
            api_major: NVN2_API_MAJOR,
            api_minor: 30,
            ..Settings::default()
        };
        let none = |i| info_value(i, &s, Platform::Ounce, |_| None);
        assert_eq!(INFO2.len(), 0xAA);
        assert_eq!(none(0), Info::Value(231));
        assert_eq!(none(1), Info::Value(30));
        assert_eq!(none(0x2E), Info::Value(201));
        assert_eq!(none(0x30), Info::Value(20));
        assert_eq!(none(0x1A), Info::Value(16384));
        assert_eq!(none(0x67), Info::Value(256));
        assert_eq!(none(0xA9), Info::Value(1));
        assert_eq!(none(0xAA), Info::Unknown);
        // A run-time item keeps the Switch value below 0x60 and is not known above it.
        assert_eq!(none(0x17), Info::Value(2));
        assert_eq!(none(0x62), Info::Runtime);
        let sdk = |i| info_value(i, &s, Platform::Ounce, |item| (item == 0x30).then_some(17));
        assert_eq!(sdk(0x30), Info::Value(17));
        assert_eq!(
            sdk(1),
            Info::Value(30),
            "item 1 is the settings', which the host took from the sdk"
        );
    }

    #[test]
    fn the_driver_switch_is_found_from_its_instructions() {
        // At 0x1000: 8 words of padding, then cmp w1, #0xa9 / mov w0, #231 / adr x9, +0x40 / adrp x10, 0 /
        // add x10, x10, #0x800 / ldrb w11, [x10, x1] / add x9, x9, x11, lsl #2 / br x9, then padding.
        let mut w = vec![0xD503_201Fu32; 8];
        w.extend([
            0x7102_A43F,
            0x5280_1CE0,
            0x1000_0209,
            0x9000_000A,
            0x9120_014A,
            0x3861_694B,
            0x8B0B_0929,
            0xD61F_0120,
        ]);
        w.extend([0xD503_201F; 12]);
        let bytes: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
        let found = find_driver_switch(&bytes, 0x1000, (0x1800, 0x2000));
        assert_eq!(
            found,
            Some(DriverSwitch {
                table: 0x1800,
                cases: 0x1000 + 10 * 4 + 0x40,
                last: 0xA9
            })
        );
        assert_eq!(
            find_driver_switch(&bytes, 0x1000, (0x1900, 0x2000)),
            None,
            "the table lies outside the data"
        );
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
