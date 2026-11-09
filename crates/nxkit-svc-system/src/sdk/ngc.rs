//! `nn::ngc::ProfanityFilter` take-overs.
//!
//! The word list lives in a system data save that the host does not mount. Nothing is profanity and nothing is masked.
//! The class is polymorphic and games call it through its vtable, so the constructor gives the object a host vtable.
//! The host vtable copies the two header words of the vtable of the game (`_ZTVN2nn3ngc15ProfanityFilterE`). Each slot
//! that points at a method that this module takes over gets the host function, and any other slot returns 0.
//! The table ends at the first slot below 0x10000 or after 40 slots. The engine also copies the slots that point into
//! another guest module, and this module does not: a slot of a table in a foreign module has no owner to ask.

use super::{get_u64, log, nothing, put_u64, zero};
use crate::event::Block;
use crate::seams::guest_sym;
use std::sync::OnceLock;

const MAX_SLOTS: u64 = 40;
static TABLE: Block = Block::new();
static VTABLE: OnceLock<u64> = OnceLock::new();

const MASK_UTF8: &str = "_ZN2nn3ngc15ProfanityFilter24MaskProfanityWordsInTextEPiPcj";
const MASK_UTF16: &str = "_ZN2nn3ngc15ProfanityFilter24MaskProfanityWordsInTextEPiPtj";
const MASK_CHAR16: &str = "_ZN2nn3ngc15ProfanityFilter24MaskProfanityWordsInTextEPiPDsj";
const CHECK_UTF8: &str = "_ZN2nn3ngc15ProfanityFilter19CheckProfanityWordsEPjjPPKcm";
const CHECK_CHAR16: &str = "_ZN2nn3ngc15ProfanityFilter19CheckProfanityWordsEPjjPPKDsm";
const CHECK_UTF16: &str = "_ZN2nn3ngc15ProfanityFilter19CheckProfanityWordsEPjjPPKtm";

/// `CheckProfanityWords(unsigned*, unsigned, const char**, size_t)`: no word is profanity.
extern "C" fn check(_this: u64, masks: u64, _pattern: u32, _words: u64, n: u64) -> u32 {
    if masks != 0 {
        super::put_zero(masks, n.saturating_mul(4).min(0x10_0000));
    }
    0
}

/// `MaskProfanityWordsInText(int*, text*, unsigned)`: nothing is masked.
extern "C" fn mask(_this: u64, masked: u64, _text: u64, _region: u32) -> u32 {
    super::put_i32(masked, 0);
    0
}

/// The methods that get a host version, by name.
fn host_methods() -> [(&'static str, usize); 9] {
    [
        (
            "_ZN2nn3ngc15ProfanityFilterD1Ev",
            nothing as *const () as usize,
        ),
        (
            "_ZN2nn3ngc15ProfanityFilterD0Ev",
            nothing as *const () as usize,
        ),
        (
            "_ZN2nn3ngc15ProfanityFilter8FinalizeEv",
            nothing as *const () as usize,
        ),
        (CHECK_UTF8, check as *const () as usize),
        (CHECK_CHAR16, check as *const () as usize),
        (CHECK_UTF16, check as *const () as usize),
        (MASK_UTF8, mask as *const () as usize),
        (MASK_CHAR16, mask as *const () as usize),
        (MASK_UTF16, mask as *const () as usize),
    ]
}

fn build() -> u64 {
    let Some(sdk) = guest_sym("_ZTVN2nn3ngc15ProfanityFilterE") else {
        log("ngc: the game has no ProfanityFilter vtable symbol, objects keep none");
        return 0;
    };
    let base = TABLE.addr((MAX_SLOTS + 2) * 8);
    if base == 0 {
        return 0;
    }
    let methods = host_methods();
    let known: Vec<(Option<u64>, usize)> =
        methods.iter().map(|(n, f)| (guest_sym(n), *f)).collect();
    put_u64(base, get_u64(sdk));
    put_u64(base + 8, get_u64(sdk + 8));
    let mut slots = 0u64;
    let mut named = 0;
    while slots < MAX_SLOTS {
        let slot = get_u64(sdk + 16 + 8 * slots);
        if slot < 0x10000 {
            break;
        }
        let host = known
            .iter()
            .find(|(a, _)| *a == Some(slot))
            .map(|(_, f)| *f);
        named += usize::from(host.is_some());
        put_u64(
            base + 16 + 8 * slots,
            host.unwrap_or(zero as *const () as usize) as u64,
        );
        slots += 1;
    }
    log(&format!(
        "ngc: ProfanityFilter host vtable of {slots} slots, {named} matched to methods"
    ));
    base + 16
}

/// `ProfanityFilter::ProfanityFilter()`: the object gets the host vtable.
extern "C" fn construct(this: u64) {
    let vtable = *VTABLE.get_or_init(build);
    if vtable != 0 && this != 0 {
        put_u64(this, vtable);
    }
}

table! {
    "_ZN2nn3ngc15ProfanityFilterC1Ev" => construct,
    "_ZN2nn3ngc15ProfanityFilterD1Ev" => nothing,
    "_ZN2nn3ngc15ProfanityFilter10InitializeEPvm" => zero,
    "_ZN2nn3ngc15ProfanityFilter17GetContentVersionEv" => zero,
    "_ZN2nn3ngc15ProfanityFilter19CheckProfanityWordsEPjjPPKcm" => check,
    "_ZN2nn3ngc15ProfanityFilter24MaskProfanityWordsInTextEPiPcj" => mask,
    "_ZN2nn3ngc15ProfanityFilter24MaskProfanityWordsInTextEPiPtj" => mask,
    "_ZN2nn3ngc15ProfanityFilter24MaskProfanityWordsInTextEPiPDsj" => mask,
    "_ZN2nn3ngc15ProfanityFilter11SetMaskModeENS1_8MaskModeE" => nothing,
    "_ZN2nn3ngc15ProfanityFilter8FinalizeEv" => nothing,
}
