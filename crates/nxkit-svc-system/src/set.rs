//! The `set` and `set:sys` services: `ISettingsServer` and `ISystemSettingsServer`.
//!
//! IDs and layouts are from libnx `set.c` and `setsys.c` and from switchbrew `Settings services`. The values follow
//! nxnative `dm/host/hle_oe.c` (`GetDesiredLanguage`, the region of the language) and `dm/host/hle_sdk16.c`
//! (`GetDeviceLocationName`).
//!
//! `set`: 0 `GetLanguageCode`, 1 and 5 `GetAvailableLanguageCodes`, 2 `MakeLanguageCode`, 3 and 6
//! `GetAvailableLanguageCodeCount`, 4 `GetRegionCode`, 8 `GetQuestFlag`, 11 `GetDeviceNickname`.
//!
//! `set:sys`: 3 and 4 `GetFirmwareVersion` and `GetFirmwareVersion2`, 7 `GetLockScreenFlag`, 13
//! `GetExternalSteadyClockSourceId`, 15 `GetUserSystemClockContext`, 23 `GetColorSetId`, 35 `GetVibrationMasterVolume`,
//! 37 and 38 `GetSettingsItemValueSize` and `GetSettingsItemValue`, 47 `GetQuestFlag`, 53
//! `GetDeviceTimeZoneLocationName`, 58 `GetNetworkSystemClockContext`, 60
//! `IsUserSystemClockAutomaticCorrectionEnabled`, 62 `GetDebugModeFlag`, 69 `GetNfcEnableFlag` and 73
//! `GetWirelessLanEnableFlag`.
//!
//! nxnative has no firmware version. The version and its layout come from libnx `setsys.h` and are chosen, see R38.

use crate::config;
use crate::time;
use crate::tz;
use crate::util::{RES_FAIL, fail, padded, reply_buf, write_out};
use nxkit_abi::ipc::{Handled, Request, Response, Service};

/// The language codes in the order of `SetLanguage` of libnx.
const LANGUAGES: [&str; 18] = [
    "ja", "en-US", "fr", "de", "it", "es", "zh-CN", "ko", "nl", "pt", "ru", "zh-TW", "en-GB", "fr-CA",
    "es-419", "zh-Hans", "zh-Hant", "pt-BR",
];

/// The 8-byte `LanguageCode` of a tag such as `en-US`: the characters in a little-endian integer.
pub(crate) fn language_code(tag: &str) -> u64 {
    let mut b = [0u8; 8];
    let n = tag.len().min(7);
    b[..n].copy_from_slice(&tag.as_bytes()[..n]);
    u64::from_le_bytes(b)
}

/// The region code of a language, as nxnative `hle_oe.c` maps it: 0 Japan, 1 USA, 2 Europe, 4 Korea or China.
pub(crate) fn region_code(tag: &str) -> i32 {
    if tag.starts_with("ja") {
        0
    } else if tag.starts_with("ko") || tag.starts_with("zh") {
        4
    } else if matches!(tag, "en-US" | "fr-CA" | "es-419" | "pt-BR") {
        1
    } else {
        2
    }
}

/// The firmware version that the host reports.
const FIRMWARE: (u8, u8, u8) = (20, 5, 0);

/// The 0x100 bytes of `SetSysFirmwareVersion`.
fn firmware_version() -> [u8; 0x100] {
    let (major, minor, micro) = FIRMWARE;
    let mut b = [0u8; 0x100];
    b[0] = major;
    b[1] = minor;
    b[2] = micro;
    b[4] = 1;
    b[5] = 0;
    b[8..0x28].copy_from_slice(&padded::<0x20>("NX"));
    let display = format!("{major}.{minor}.{micro}");
    b[0x68..0x80].copy_from_slice(&padded::<0x18>(&display));
    let title = format!("NintendoSDK Firmware for NX {display}-1.0");
    b[0x80..0x100].copy_from_slice(&padded::<0x80>(&title));
    b
}

/// The root of `set` and `set:sys`.
pub(crate) struct Settings {
    pub(crate) system: bool,
}

impl Settings {
    fn user(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let lang = config().language.as_str();
        match req.command {
            0 => rsp.push_u64(language_code(lang)),
            1 | 5 => {
                let list: Vec<u8> = LANGUAGES
                    .iter()
                    .flat_map(|l| language_code(l).to_le_bytes())
                    .collect();
                let written = write_out(req, 0, &list).unwrap_or(0);
                rsp.push_u32(u32::try_from(written / 8).unwrap_or(0));
            }
            2 => {
                let index = req.u32_at(0).unwrap_or(u32::MAX) as usize;
                match LANGUAGES.get(index) {
                    Some(l) => rsp.push_u64(language_code(l)),
                    None => return fail(rsp, RES_FAIL),
                }
            }
            3 | 6 => rsp.push_u32(LANGUAGES.len() as u32),
            4 => rsp.push_u32(region_code(lang) as u32),
            8 => rsp.push_bytes(&[0]),
            11 => return reply_buf(req, 0, &padded::<0x80>(&config().nickname), rsp),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }

    fn system(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            3 | 4 => return reply_buf(req, 0, &firmware_version(), rsp),
            7 | 47 | 60 | 62 | 69 | 73 => rsp.push_bytes(&[0]),
            13 => rsp.push_bytes(&time::STEADY_ID),
            15 => rsp.push_bytes(&time::user_context()),
            23 => rsp.push_u32(0),
            35 => rsp.push_u32(1.0f32.to_bits()),
            // nxnative has no settings database, so every item is missing.
            37 | 38 => return fail(rsp, RES_FAIL),
            53 => rsp.push_bytes(&tz::location_name()),
            58 => rsp.push_bytes(&time::network_context()),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

impl Service for Settings {
    fn name(&self) -> &'static str {
        if self.system { "set:sys" } else { "set" }
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        if self.system {
            self.system(req, rsp)
        } else {
            self.user(req, rsp)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_language_code_is_the_tag_in_a_little_endian_integer() {
        assert_eq!(language_code("en-US"), 0x5355_2d6e_65);
        assert_eq!(language_code("ja"), 0x616a);
    }

    #[test]
    fn the_regions_follow_nxnative() {
        assert_eq!(region_code("ja"), 0);
        assert_eq!(region_code("ko"), 4);
        assert_eq!(region_code("zh-TW"), 4);
        assert_eq!(region_code("en-US"), 1);
        assert_eq!(region_code("de"), 2);
        assert_eq!(region_code("en-GB"), 2);
    }

    #[test]
    fn the_firmware_buffer_has_the_documented_fields() {
        let b = firmware_version();
        assert_eq!(&b[..3], &[20, 5, 0]);
        assert_eq!(&b[8..10], b"NX");
        assert_eq!(&b[0x68..0x6D], b"20.5.");
    }
}
