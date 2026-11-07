//! `nn::settings` language take-overs. Source: nxnative `dm/host/hle_sdk16.c`, section "settings".
//!
//! The table is the fixed list of `nn::settings::Language`. A Chinese code of either spelling matches both ids of its
//! script, because the older games compare with `Chinese` (6) and `Taiwanese` (11) and the newer ones with
//! `SimplifiedChinese` (15) and `TraditionalChinese` (16).

use super::{get_bytes, get_i32, put_bytes};

const CODES: [&str; 18] = [
    "ja", "en-US", "fr", "de", "it", "es", "zh-CN", "ko", "nl", "pt", "ru", "zh-TW", "en-GB", "fr-CA", "es-419",
    "zh-Hans", "zh-Hant", "pt-BR",
];

fn code_bytes(lang: i32) -> Option<[u8; 8]> {
    let text = CODES.get(usize::try_from(lang).ok()?)?;
    let mut b = [0u8; 8];
    b[..text.len()].copy_from_slice(text.as_bytes());
    Some(b)
}

fn matches(lang: i32, code: &[u8]) -> bool {
    let Some(own) = code_bytes(lang) else {
        return false;
    };
    if own[..] == *code {
        return true;
    }
    let other: &[u8] = match lang {
        6 => b"zh-Hans",
        15 => b"zh-CN",
        11 => b"zh-Hant",
        16 => b"zh-TW",
        _ => return false,
    };
    code.starts_with(other) && code.get(other.len()..).is_some_and(|rest| rest.iter().all(|&c| c == 0))
}

/// `operator==(const Language&, const LanguageCode&)`.
extern "C" fn language_equals_code(lang: u64, code: u64) -> u32 {
    u32::from(matches(get_i32(lang), &get_bytes(code, 8)))
}

/// `operator==(const LanguageCode&, const Language&)`.
extern "C" fn code_equals_language(code: u64, lang: u64) -> u32 {
    u32::from(matches(get_i32(lang), &get_bytes(code, 8)))
}

/// `LanguageCode::Make(Language)`: 8 bytes returned by value.
extern "C" fn make(lang: u32) -> u64 {
    code_bytes(lang as i32).map_or(0, u64::from_le_bytes)
}

/// `GetAvailableLanguageCodes(LanguageCode*, int)`: gives the number written.
extern "C" fn available(out: u64, count: u32) -> u32 {
    let n = usize::try_from(count as i32).unwrap_or(0).min(CODES.len());
    for lang in 0..n {
        if let Some(b) = code_bytes(lang as i32) {
            put_bytes(out + 8 * lang as u64, &b);
        }
    }
    n as u32
}

/// `GetAvailableLanguageCodeCount`.
extern "C" fn available_count() -> u32 {
    CODES.len() as u32
}

table! {
    "_ZN2nn8settingseqERKNS0_8LanguageERKNS0_12LanguageCodeE" => language_equals_code,
    "_ZN2nn8settingseqERKNS0_12LanguageCodeERKNS0_8LanguageE" => code_equals_language,
    "_ZN2nn8settings12LanguageCode4MakeENS0_8LanguageE" => make,
    "_ZN2nn8settings25GetAvailableLanguageCodesEPNS0_12LanguageCodeEi" => available,
    "_ZN2nn8settings29GetAvailableLanguageCodeCountEv" => available_count,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chinese_code_matches_both_spellings() {
        assert!(matches(6, b"zh-Hans\0"));
        assert!(matches(15, b"zh-CN\0\0\0"));
        assert!(matches(11, b"zh-Hant\0"));
        assert!(!matches(6, b"zh-Hant\0"));
    }

    #[test]
    fn make_gives_the_code_of_the_language() {
        assert_eq!(make(1).to_le_bytes(), *b"en-US\0\0\0");
        assert_eq!(make(99), 0);
    }
}
