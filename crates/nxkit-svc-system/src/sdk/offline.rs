//! The entries of the engine `dm/host/hle_offline.c` that need the host: local wireless and Bluetooth LE events, the
//! error applet, the web page applets and the album calls that log.
//!
//! The events are made with the `event` module, which calls `CreateSystemEvent` of the game, so they live here and
//! not in `nxkit-svc-offline`. A bound event never fires. The error applet opens a library applet for the length of the
//! call with `AppletGuard` and logs what the game asks to show. No page and no picture is shown. The web page applets
//! and the album calls log and fail. Every log line has the text of the engine.

use super::oe_state::AppletGuard;
use super::{RES_FAIL, log};
use crate::event;

/// Binds the event at `ev` as a manual-clear kernel event that nothing signals.
fn bind_event(ev: u64) {
    event::bind(ev, false);
}

/// `ldn::AttachStateChangeEvent(SystemEventType*)`: local wireless state event: bound, never fires.
extern "C" fn ldn_attach_state_change_event(ev: u64) {
    bind_event(ev);
}

/// `bluetooth::AcquireBle*Event(SystemEventType*)`: the BLE scan, connection, discovery, MTU and GATT events: bound,
/// never fire.
extern "C" fn ble_event(ev: u64) -> u32 {
    bind_event(ev);
    0
}

fn code_text(code: u64) -> String {
    format!("{:04}-{:04}", code & 0xffff_ffff, code >> 32)
}

/// `err::ShowError(ErrorCode)`: error display applet; the host only logs it. The `ErrorCode` is the category in the
/// low 32 bits and the number in the high 32 bits.
extern "C" fn show_error_code(code: u64) {
    let _applet = AppletGuard::new("error");
    log(&format!("game asks to show an error: {}", code_text(code)));
}

/// `err::ShowError(const ErrorResultVariant&)`: error display applet; the host only logs it.
extern "C" fn show_error_variant() {
    let _applet = AppletGuard::new("error");
    log("game asks to show an error (ErrorResultVariant)");
}

/// `err::ShowUnacceptableAddOnContentVersionError()`: add-on content version mismatch error applet; the host only
/// logs it.
extern "C" fn show_add_on_content_version() {
    let _applet = AppletGuard::new("error");
    log("game asks to show an error: add-on content version mismatch");
}

/// `err::ShowUnacceptableApplicationVersionError()`: game version mismatch error applet; the host only logs it.
extern "C" fn show_application_version() {
    let _applet = AppletGuard::new("error");
    log("game asks to show an error: game version mismatch");
}

/// `err::ShowApplicationError(const ApplicationErrorArg&)`: application error applet; the host only logs it. The
/// error screen of the game itself goes through the error applet too (Pokémon Legends: Z-A, SDK 19).
extern "C" fn show_application_error() {
    let _applet = AppletGuard::new("error");
    log("game asks to show an application error (ApplicationErrorArg)");
}

/// `err::ShowNetworkServiceError(ErrorCode)`: network service error applet; the host only logs it. It follows a
/// failed online call (Pokémon Sword, SDK 7.3.2).
extern "C" fn show_network_service_error(code: u64) {
    let _applet = AppletGuard::new("error");
    log(&format!(
        "game asks to show a network service error: {}",
        code_text(code)
    ));
}

/// `web::ShowLobbyPage(LobbyPageReturnValue*, const ShowLobbyPageArg&)`: online lobby web applet (cut).
extern "C" fn show_lobby_page() -> u32 {
    log("web::ShowLobbyPage (cut)");
    RES_FAIL
}

/// `web::ShowWebPage(WebPageReturnValue*, const ShowWebPageArg&)`: web page applet (cut). The game constructs the
/// return value itself, so it stays as constructed (Tears of the Kingdom).
extern "C" fn show_web_page() -> u32 {
    log("web::ShowWebPage (cut)");
    RES_FAIL
}

/// `album::SaveScreenshot(const void*, size_t, ImageSize, ImageOrientation, AlbumReportOption)` and the SDK 7
/// overload without an orientation (DRAGON QUEST XI S): save a picture to the album: no album, fails.
extern "C" fn save_screenshot() -> u32 {
    log("album::SaveScreenshot (no album)");
    RES_FAIL
}

/// `album::SaveCurrentScreenshot(AlbumReportOption)`: screenshot of the current frame (capsrv): no album, fails
/// (Pokémon Legends: Z-A, SDK 19).
extern "C" fn save_current_screenshot() -> u32 {
    log("album::SaveCurrentScreenshot (no album)");
    RES_FAIL
}

table! {
    "_ZN2nn3ldn22AttachStateChangeEventEPNS_2os15SystemEventTypeE" => ldn_attach_state_change_event,
    "_ZN2nn9bluetooth19AcquireBleScanEventEPNS_2os15SystemEventTypeE" => ble_event,
    "_ZN2nn9bluetooth37AcquireBleConnectionStateChangedEventEPNS_2os15SystemEventTypeE" => ble_event,
    "_ZN2nn9bluetooth31AcquireBleServiceDiscoveryEventEPNS_2os15SystemEventTypeE" => ble_event,
    "_ZN2nn9bluetooth24AcquireBleMtuConfigEventEPNS_2os15SystemEventTypeE" => ble_event,
    "_ZN2nn9bluetooth28AcquireBleGattOperationEventEPNS_2os15SystemEventTypeE" => ble_event,
    "_ZN2nn3err9ShowErrorENS0_9ErrorCodeE" => show_error_code,
    "_ZN2nn3err9ShowErrorERKNS0_18ErrorResultVariantE" => show_error_variant,
    "_ZN2nn3err40ShowUnacceptableAddOnContentVersionErrorEv" => show_add_on_content_version,
    "_ZN2nn3err39ShowUnacceptableApplicationVersionErrorEv" => show_application_version,
    "_ZN2nn3err20ShowApplicationErrorERKNS0_19ApplicationErrorArgE" => show_application_error,
    "_ZN2nn3err23ShowNetworkServiceErrorENS0_9ErrorCodeE" => show_network_service_error,
    "_ZN2nn3web13ShowLobbyPageEPNS0_20LobbyPageReturnValueERKNS0_16ShowLobbyPageArgE" => show_lobby_page,
    "_ZN2nn3web11ShowWebPageEPNS0_18WebPageReturnValueERKNS0_14ShowWebPageArgE" => show_web_page,
    "_ZN2nn5album14SaveScreenshotEPKvmNS0_9ImageSizeENS0_16ImageOrientationENS0_17AlbumReportOptionE" => save_screenshot,
    "_ZN2nn5album14SaveScreenshotEPKvmNS0_9ImageSizeENS0_17AlbumReportOptionE" => save_screenshot,
    "_ZN2nn5album21SaveCurrentScreenshotENS0_17AlbumReportOptionE" => save_current_screenshot,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_code_prints_category_and_number_in_four_digits() {
        assert_eq!(code_text((2124u64 << 32) | 4007), "4007-2124");
        assert_eq!(code_text(7), "0007-0000");
    }
}
