//! `nn::oe` and `nn::applet` take-overs. Source: nxnative `dm/host/hle_oe.c`. The notification queue, the focus state
//! and the library applet counter are in `oe_state.rs`.
//!
//! The functions that nxnative leaves empty because the host has no screenshot, recording or HOME button service are
//! empty here too. The rest answers from the shared state of the applet services and from the `Config`.

use super::{get_bytes, log, log_once, log_once_with, nothing, put_bytes, put_i32, put_u64, zero};
use crate::config;
use crate::event;
use crate::seams::seams;
use crate::set::{language_code, region_code};
use crate::state::state;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Game programs that ask for a graphics frame length, for the log.
static LAST_FRAME_LENGTH: AtomicU32 = AtomicU32::new(u32::MAX);
static NO_SLEEP: AtomicU32 = AtomicU32::new(0);
static LAUNCH_POPPED: AtomicBool = AtomicBool::new(false);
static DEMO_EXIT_LOGGED: AtomicBool = AtomicBool::new(false);
static REGION_LOGGED: AtomicBool = AtomicBool::new(false);

/// `ExitApplicationAndGoBackToInStoreDemoMenu`: the host has no demo menu.
extern "C" fn demo_exit() {
    log_once(
        &DEMO_EXIT_LOGGED,
        "oe: ExitApplicationAndGoBackToInStoreDemoMenu called: ignored (no in-store demo menu)",
    );
}

/// `SetUserInactivityDetectionTimeExtendedUnsafe`: mode 2 or 0.
extern "C" fn inactivity_unsafe(on: u32) {
    state().with(|i| i.idle_extension = if on & 1 != 0 { 2 } else { 0 });
}

/// `SetUserInactivityDetectionTimeExtended`: mode 1 or 0.
extern "C" fn inactivity_safe(on: u32) {
    state().with(|i| i.idle_extension = on & 1);
}

/// `GetUserInactivityDetectionTimeExtensionMode`.
extern "C" fn inactivity_mode() -> u32 {
    state().with(|i| i.idle_extension)
}

/// `IsUserInactivityDetectionTimeExtended`.
extern "C" fn inactivity_extended() -> u32 {
    u32::from(state().with(|i| i.idle_extension) != 0)
}

/// `nn::applet::GetAppletResourceUserId`. The SDK 20 audio renderer aborts on 0 (nxnative `hle_oe.c`).
extern "C" fn aruid() -> u64 {
    0x80
}

/// `TryPopLaunchParameter(size_t*, void*, size_t)`: the parameter of the `Config`, once.
extern "C" fn pop_launch_parameter(out: u64, buf: u64, n: u64) -> u32 {
    put_u64(out, 0);
    let Some(parameter) = config().launch_parameter.as_ref() else {
        return 0;
    };
    if LAUNCH_POPPED.swap(true, Ordering::SeqCst) {
        return 0;
    }
    let len = parameter
        .len()
        .min(usize::try_from(n).unwrap_or(usize::MAX));
    put_bytes(buf, &parameter[..len]);
    put_u64(out, len as u64);
    log(&format!("oe::TryPopLaunchParameter: {len} bytes"));
    1
}

/// `GetPreviousProgramIndex`: the program that started this one, else -1 for the HOME menu.
extern "C" fn previous_program() -> i32 {
    config().previous_program.unwrap_or(-1)
}

/// `ExecuteProgram(int, const void*, size_t)`: ends this program. The host starts the next one through the seam, or
/// the process just ends. The call does not return.
extern "C" fn execute_program(index: u32, param: u64, n: u64) {
    let len = usize::try_from(n).unwrap_or(0).min(0x10_0000);
    let bytes = if param != 0 && len > 0 {
        get_bytes(param, len)
    } else {
        Vec::new()
    };
    let index = index as i32;
    match seams().execute_program {
        Some(start) => {
            log(&format!(
                "oe::ExecuteProgram({index}, {} bytes): this program ends",
                bytes.len()
            ));
            start(index, &bytes);
        }
        None => log(&format!(
            "oe::ExecuteProgram({index}, {} bytes): the host has only this program, it ends",
            bytes.len()
        )),
    }
    super::lifecycle::quit("oe::ExecuteProgram");
    loop {
        std::thread::park();
    }
}

extern "C" fn sleep_begin() {
    NO_SLEEP.fetch_add(1, Ordering::SeqCst);
    state().with(|i| i.auto_sleep_disabled = true);
}

extern "C" fn sleep_end() {
    let _ = NO_SLEEP.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
    let open = NO_SLEEP.load(Ordering::SeqCst) > 0;
    state().with(|i| i.auto_sleep_disabled = open);
}

extern "C" fn sleep_disabled() -> u32 {
    u32::from(NO_SLEEP.load(Ordering::SeqCst) > 0)
}

extern "C" fn cpu_overclock() -> u32 {
    u32::from(state().with(|i| i.cpu_overclock))
}

extern "C" fn set_cpu_overclock(on: u32) {
    state().with(|i| i.cpu_overclock = on & 0xff != 0);
}

extern "C" fn operation_mode() -> u32 {
    state().operation_mode()
}

extern "C" fn performance_mode() -> u32 {
    state().performance_mode()
}

/// `GetCurrentGraphicsScale`: 0 handheld, 1 console.
extern "C" fn graphics_scale() -> u32 {
    state().operation_mode()
}

/// `gs::sdk::SetGraphicsFrameLengthMode`: noted.
extern "C" fn frame_length(mode: u32) {
    if LAST_FRAME_LENGTH.swap(mode, Ordering::Relaxed) != mode {
        log(&format!(
            "gs: graphics frame length mode {mode} (0 auto, 1 30 fps, 2 60 fps, 3 120 fps)"
        ));
    }
}

/// `GetDefaultDisplayResolution(int*, int*)`: 1920x1080 docked, 1280x720 handheld.
extern "C" fn default_resolution(w: u64, h: u64) {
    let docked = state().operation_mode() == 1;
    put_i32(w, if docked { 1920 } else { 1280 });
    put_i32(h, if docked { 1080 } else { 720 });
}

/// `GetDefaultDisplayResolutionChangeEvent(SystemEvent*)`: bound, never signalled.
extern "C" fn resolution_event(ev: u64) {
    event::bind(ev, false);
}

/// `GetPerformanceConfiguration(PerformanceMode)`: Normal 0x00010000, Boost 0x00010001.
extern "C" fn performance_configuration(mode: u32) -> u32 {
    if mode == 1 { 0x0001_0001 } else { 0x0001_0000 }
}

/// `GetDesiredLanguage`: the 8 bytes of the language code in the return register.
extern "C" fn desired_language() -> u64 {
    language_code(&config().language)
}

/// `GetRegionCode`.
extern "C" fn region() -> i32 {
    let r = region_code(&config().language);
    log_once_with(&REGION_LOGGED, || {
        format!("oe::GetRegionCode: {r} for language {}", config().language)
    });
    r
}

/// `GetPseudoDeviceId(Uuid*)`.
extern "C" fn pseudo_device_id(uuid: u64) {
    put_bytes(uuid, b"xbl\0SWNATIVE1001");
}

table! {
    "_ZN2nn2oe32BeginPresentPhotographedContentsEv" => nothing,
    "_ZN2nn2oe30EndPresentPhotographedContentsEv" => nothing,
    "_ZN2nn2oe41ExitApplicationAndGoBackToInStoreDemoMenuEv" => demo_exit,
    "_ZN2nn2oe17FinishStartupLogoEv" => nothing,
    "_ZN2nn2oe44SetUserInactivityDetectionTimeExtendedUnsafeEb" => inactivity_unsafe,
    "_ZN2nn2oe38SetUserInactivityDetectionTimeExtendedEb" => inactivity_safe,
    "_ZN2nn2oe43GetUserInactivityDetectionTimeExtensionModeEv" => inactivity_mode,
    "_ZN2nn2oe15EnableRecordingEv" => nothing,
    "_ZN2nn6applet23GetAppletResourceUserIdEv" => aruid,
    "_ZN2nn2oe21TryPopLaunchParameterEPmPvm" => pop_launch_parameter,
    "_ZN2nn2oe23GetPreviousProgramIndexEv" => previous_program,
    "_ZN2nn2oe14ExecuteProgramEiPKvm" => execute_program,
    "_ZN2nn2oe16DisableRecordingEv" => nothing,
    "_ZN2nn2oe13MuteRecordingEv" => nothing,
    "_ZN2nn2oe15UnmuteRecordingEv" => nothing,
    "_ZN2nn2oe16EnableScreenShotEv" => nothing,
    "_ZN2nn2oe17DisableScreenShotEv" => nothing,
    "_ZN2nn2oe23BeginBlockingHomeButtonEv" => nothing,
    "_ZN2nn2oe21EndBlockingHomeButtonEv" => nothing,
    "_ZN2nn2oe29SetScreenShotImageOrientationENS_5album16ImageOrientationE" => nothing,
    "_ZN2nn2oe29BeginAutoSleepDisabledSectionEv" => sleep_begin,
    "_ZN2nn2oe27EndAutoSleepDisabledSectionEv" => sleep_end,
    "_ZN2nn2oe19IsAutoSleepDisabledEv" => sleep_disabled,
    "_ZN2nn2oe37IsUserInactivityDetectionTimeExtendedEv" => inactivity_extended,
    "_ZN2nn2oe30InitializeCopyrightFrameBufferEPvm" => nothing,
    "_ZN2nn2oe17SetCopyrightImageEPKvmiiiiNS0_16WindowOriginModeE" => nothing,
    "_ZN2nn2oe22SetCopyrightVisibilityEb" => nothing,
    "_ZN2nn2oe16GetOperationModeEv" => operation_mode,
    "_ZN2nn2oe18GetPerformanceModeEv" => performance_mode,
    "_ZN2nn2oe21IsCpuOverclockEnabledEv" => cpu_overclock,
    "_ZN2nn2oe22SetCpuOverclockEnabledEb" => set_cpu_overclock,
    "_ZN2nn2oe23GetCurrentGraphicsScaleEv" => graphics_scale,
    "_ZN2nn2oe42SetGraphicsScaleChangedNotificationEnabledEb" => nothing,
    "_ZN2nn2gs3sdk26SetGraphicsFrameLengthModeENS1_23GraphicsFrameLengthModeE" => frame_length,
    "_ZN2nn2oe27GetDefaultDisplayResolutionEPiS1_" => default_resolution,
    "_ZN2nn2oe38GetDefaultDisplayResolutionChangeEventEPNS_2os11SystemEventE" => resolution_event,
    "_ZN2nn2oe40SendServerMaintenanceOverlayNotificationENS_4time9PosixTimeES2_" => nothing,
    "_ZN2nn2oe27GetPerformanceConfigurationENS0_15PerformanceModeE" => performance_configuration,
    "_ZN2nn2oe24SetAlbumImageOrientationENS_5album16ImageOrientationE" => nothing,
    "_ZN2nn2oe34DisableAlbumImageTakenNotificationEv" => nothing,
    "_ZN2nn2oe33EnableAlbumImageTakenNotificationEv" => nothing,
    "_ZN2nn2oe24IsInStoreDemoModeEnabledEv" => zero,
    "_ZN2nn2oe27SetPerformanceConfigurationENS0_15PerformanceModeEi" => nothing,
    "_ZN2nn2oe18GetDesiredLanguageEv" => desired_language,
    "_ZN2nn2oe13GetRegionCodeEv" => region,
    "_ZN2nn2oe26IsLanguageSelectionLimitedEv" => zero,
    "_ZN2nn2oe17GetPseudoDeviceIdEPNS_4util4UuidE" => pseudo_device_id,
}
