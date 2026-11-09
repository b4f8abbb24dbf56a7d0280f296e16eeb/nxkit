//! `nn::swkbd` take-overs: the software keyboard and the inline keyboard. Source: nxnative `dm/host/hle_swkbd.c`.
//!
//! `ShowKeyboard` reads the `ShowKeyboardArg` that `MakePreset` and `SetInitialTextUtf8` wrote in the SDK, asks the
//! front end for the text and writes it back to the `String` as the applet does. The offsets are the ones of the SDK
//! that nxnative was written against (`ShowKeyboard`, `SetInitialTextUtf8` and `ReadCloseResultAndString`). The text
//! comes from the `text_input` seam. Without it the keyboard is
//! cancelled, which is what the SDK returns for a closed applet.
//!
//! The inline keyboard starts no applet. `Appear` arms one request and the next `Calc` asks for the text and calls the
//! callbacks of the game on the guest thread that called `Calc`. The naming panel and the character name rule of
//! nxnative (`hle_charname.c`) and the freeze of the guest clock while the box is open are not part of this crate.

use super::oe_state::AppletGuard;
use super::{get_bytes, get_i32, get_u8, get_u64, log, nothing, put_bytes, put_zero, zero};
use crate::event::Block;
use crate::seams::{TextRequest, seams};
use crate::sys::call_guest;
use crate::util::lock;
use std::sync::Mutex;

const ARG_HEADER: u64 = 0x24;
const ARG_SUB: u64 = 0xa6;
const ARG_GUIDE: u64 = 0x1a8;
const ARG_MAXLEN: u64 = 0x3ac;
const ARG_MINLEN: u64 = 0x3b0;
const ARG_UTF8: u64 = 0x3bd;
const ARG_INIT_OFF: u64 = 0x3c0;
const ARG_INIT_LEN: u64 = 0x3c4;
const ARG_WORKBUF: u64 = 0x4d0;
const HEADER_UNITS: usize = 65;
const SUB_UNITS: usize = 129;
const GUIDE_UNITS: usize = 257;
/// Module 159, description 1: what the SDK returns when the close result is not OK.
const CANCELED: u32 = 0x29F;
/// The most units that the applet returns, and the size of the output of `Calc` with its terminator.
const TEXT_UNITS: usize = 1002;
const INLINE_UNITS: usize = 500;

static CHANGED_ARG: Block = Block::new();

/// UTF-16 units at `addr` (at most `n`, stops at 0) as a string.
fn read_utf16(addr: u64, n: usize) -> String {
    let b = get_bytes(addr, n * 2);
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// A string as UTF-16 units, at most `max_units`, without splitting a surrogate pair.
fn to_utf16(text: &str, max_units: usize) -> Vec<u16> {
    let mut out = Vec::new();
    for c in text.chars() {
        let need = c.len_utf16();
        if out.len() + need > max_units {
            break;
        }
        let mut b = [0u16; 2];
        out.extend_from_slice(c.encode_utf16(&mut b));
    }
    out
}

/// UTF-8 bytes of `units` that fit `cap` bytes with the terminating 0, without splitting a character.
fn utf16_to_utf8(units: &[u16], cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for c in char::decode_utf16(units.iter().copied()) {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        let mut b = [0u8; 4];
        let s = c.encode_utf8(&mut b);
        if out.len() + s.len() + 1 > cap {
            break;
        }
        out.extend_from_slice(s.as_bytes());
    }
    out
}

/// The text of the player for a request, from the front end.
fn ask_text(req: &TextRequest) -> Option<String> {
    (seams().text_input?)(req)
}

/// `ShowKeyboard(String*, const ShowKeyboardArg&)`.
extern "C" fn show_keyboard(out: u64, arg: u64) -> u32 {
    let _applet = AppletGuard::new("software keyboard");
    let max_len = get_i32(arg + ARG_MAXLEN);
    let min_len = get_i32(arg + ARG_MINLEN);
    let init_len = get_i32(arg + ARG_INIT_LEN);
    let init_off = get_i32(arg + ARG_INIT_OFF);
    let work = get_u64(arg + ARG_WORKBUF);
    let initial = if init_len > 0 && work != 0 {
        read_utf16(work + u64::from(init_off as u32), init_len as usize)
    } else {
        String::new()
    };
    let req = TextRequest {
        header: read_utf16(arg + ARG_HEADER, HEADER_UNITS),
        sub: read_utf16(arg + ARG_SUB, SUB_UNITS),
        guide: read_utf16(arg + ARG_GUIDE, GUIDE_UNITS),
        initial,
        max_len: max_len.max(0) as u32,
        min_len: min_len.max(0) as u32,
    };
    let text = ask_text(&req);
    log(&format!(
        "swkbd: title '{}' initial '{}' limit {max_len} min {min_len} -> {}",
        req.header,
        req.initial,
        match &text {
            Some(t) => format!("OK '{t}'"),
            None => "cancel".to_owned(),
        }
    ));
    let Some(text) = text else {
        return CANCELED;
    };
    let (buf, cap) = (get_u64(out), get_u64(out + 8));
    if buf == 0 || cap == 0 {
        return 0;
    }
    put_zero(buf, cap);
    let units = to_utf16(
        &text,
        if max_len > 0 {
            max_len as usize
        } else {
            TEXT_UNITS
        },
    );
    if get_u8(arg + ARG_UTF8) != 0 {
        put_bytes(buf, &utf16_to_utf8(&units, cap as usize));
    } else {
        let n = if (units.len() as u64 + 1) * 2 <= cap {
            units.len()
        } else {
            (cap / 2).saturating_sub(1) as usize
        };
        let bytes: Vec<u8> = units.iter().take(n).flat_map(|u| u.to_le_bytes()).collect();
        put_bytes(buf, &bytes);
    }
    0
}

/// One inline keyboard, found by the address of its object.
#[derive(Clone, Copy, Default)]
struct Slot {
    kb: u64,
    cancel: u64,
    fin_init: u64,
    released: u64,
    finished: u64,
    enter: u64,
    changed: u64,
    pending: bool,
    fin_init_due: bool,
    released_due: bool,
    finished_due: bool,
    max_len: i32,
    state: i32,
}

static SLOTS: Mutex<[Slot; 4]> = Mutex::new(
    [Slot {
        kb: 0,
        cancel: 0,
        fin_init: 0,
        released: 0,
        finished: 0,
        enter: 0,
        changed: 0,
        pending: false,
        fin_init_due: false,
        released_due: false,
        finished_due: false,
        max_len: 0,
        state: 0,
    }; 4],
);

fn slot_index(slots: &mut [Slot; 4], kb: u64) -> Option<usize> {
    slots.iter().position(|s| s.kb == kb).or_else(|| {
        slots
            .iter()
            .position(|s| s.kb == 0)
            .inspect(|&i| slots[i].kb = kb)
    })
}

/// Runs `f` on the slot of `kb`, which the first use claims. Gives `None` when the four slots are taken.
fn with_slot<R>(kb: u64, f: impl FnOnce(&mut Slot) -> R) -> Option<R> {
    let mut slots = lock(&SLOTS);
    let i = slot_index(&mut slots, kb)?;
    Some(f(&mut slots[i]))
}

/// `InlineKeyboard::Initialize(void*)`: no applet starts. The finished-initialize callback follows on the next `Calc`.
extern "C" fn inline_initialize(kb: u64, _work: u64) -> u32 {
    let ok = with_slot(kb, |s| s.fin_init_due = true).is_some();
    log(&format!(
        "swkbd: inline keyboard Initialize {kb:#x} (no applet starts, Appear asks for the text like ShowKeyboard){}",
        if ok { "" } else { " (registration table full)" }
    ));
    1
}

/// `InlineKeyboard::Finalize()` and the destructor.
extern "C" fn inline_finalize(kb: u64) {
    let mut slots = lock(&SLOTS);
    for s in slots.iter_mut().filter(|s| s.kb == kb) {
        *s = Slot::default();
    }
}

macro_rules! callback_setter {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(kb: u64, f: u64) {
            with_slot(kb, |s| s.$field = f);
        }
    };
}

callback_setter!(set_cancel, cancel);
callback_setter!(set_enter, enter);
callback_setter!(set_changed, changed);
callback_setter!(set_finished, finished);
callback_setter!(set_fin_init, fin_init);
callback_setter!(set_released, released);

/// `InlineKeyboard::Appear(const AppearArg&)`: the next `Calc` asks for the text. The maximum length is at +0x20.
extern "C" fn inline_appear(kb: u64, arg: u64) {
    let max = if arg != 0 { get_i32(arg + 0x20) } else { 0 };
    with_slot(kb, |s| {
        s.pending = true;
        s.state = 3;
        s.max_len = if max > 0 && max <= INLINE_UNITS as i32 {
            max
        } else {
            0
        };
    });
    log(&format!(
        "swkbd: inline keyboard Appear {kb:#x} (at most {max} characters), text asked for on the next tick"
    ));
}

/// `InlineKeyboard::Disappear()`: the finished callback follows on the next `Calc`.
extern "C" fn inline_disappear(kb: u64) {
    with_slot(kb, |s| {
        s.pending = false;
        s.finished_due = true;
        if s.state != 0 {
            s.state = 1;
        }
    });
}

/// `InlineKeyboard::UnsetUserWordInfo()`: the released callback follows on the next `Calc`.
extern "C" fn inline_unset_words(kb: u64) -> u32 {
    with_slot(kb, |s| s.released_due = true);
    0
}

/// The callbacks and request that `Calc` runs after it released the lock.
#[derive(Default)]
struct Due {
    init: u64,
    released: u64,
    finished: u64,
    cancel: u64,
    enter: u64,
    changed: u64,
    max: i32,
    ask: bool,
    state: i32,
}

fn take_due(kb: u64) -> Due {
    let mut slots = lock(&SLOTS);
    let Some(i) = slot_index(&mut slots, kb) else {
        return Due::default();
    };
    let s = &mut slots[i];
    let mut d = Due::default();
    if s.fin_init_due {
        if s.state == 0 {
            s.state = 1;
        }
        if s.fin_init != 0 {
            s.fin_init_due = false;
            d.init = s.fin_init;
        }
    }
    if s.released_due && s.released != 0 {
        s.released_due = false;
        d.released = s.released;
    }
    if s.finished_due {
        s.finished_due = false;
        d.finished = s.finished;
    }
    if s.pending {
        s.pending = false;
        d.ask = true;
        d.cancel = s.cancel;
        d.enter = s.enter;
        d.changed = s.changed;
        d.max = s.max_len;
    }
    d.state = s.state;
    d
}

fn call(f: u64, arg: u64) {
    if f != 0 {
        call_guest(f, [arg, 0, 0, 0, 0, 0]);
    }
}

/// `InlineKeyboard::Calc()`: gives the state of the keyboard. 0 until the initialization has finished, 1 ready or
/// hidden, 3 shown.
extern "C" fn inline_calc(kb: u64) -> u32 {
    let d = take_due(kb);
    call(d.init, 0);
    call(d.released, 0);
    call(d.finished, 0);
    if !d.ask {
        return d.state as u32;
    }
    let text = if d.enter != 0 {
        ask_text(&TextRequest {
            max_len: d.max.max(0) as u32,
            ..TextRequest::default()
        })
    } else {
        None
    };
    log(&format!(
        "swkbd: inline keyboard {kb:#x} -> {}",
        match (&text, d.enter) {
            (Some(t), _) => format!("OK '{t}'"),
            (None, 0) => "no enter callback, cancel".to_owned(),
            (None, _) => "cancel".to_owned(),
        }
    ));
    with_slot(kb, |s| s.state = 1);
    let Some(text) = text else {
        call(d.cancel, 0);
        return 1;
    };
    let a = CHANGED_ARG.addr(0x1000);
    if a == 0 {
        return 1;
    }
    // ChangedStringArg and DecidedEnterArg: the text inline, then its length, the dictionary range and the cursor.
    put_zero(a, 0x400);
    let units = to_utf16(
        &text,
        if d.max > 0 {
            d.max as usize
        } else {
            INLINE_UNITS
        },
    );
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    put_bytes(a, &bytes);
    put_bytes(a + 0x3ec, &(units.len() as i32).to_le_bytes());
    put_bytes(a + 0x3f8, &(units.len() as i32).to_le_bytes());
    call(d.changed, a);
    call(d.enter, a);
    1
}

/// `InlineKeyboard::GetMaxHeight()`: 400 of the 720 lines.
extern "C" fn max_height(_kb: u64) -> u32 {
    400
}

/// `InlineKeyboard::GetWindowSize(int*, int*) const`: 1280 x 720, as on the console.
extern "C" fn window_size(_kb: u64, w: u64, h: u64) {
    super::put_i32(w, 1280);
    super::put_i32(h, 720);
}

table! {
    "_ZN2nn5swkbd12ShowKeyboardEPNS0_6StringERKNS0_15ShowKeyboardArgE" => show_keyboard,
    "_ZN2nn5swkbd14InlineKeyboard10InitializeEPv" => inline_initialize,
    "_ZN2nn5swkbd14InlineKeyboard8FinalizeEv" => inline_finalize,
    "_ZN2nn5swkbd14InlineKeyboardD1Ev" => inline_finalize,
    "_ZN2nn5swkbd14InlineKeyboard24SetDecidedCancelCallbackEPFvvE" => set_cancel,
    "_ZN2nn5swkbd14InlineKeyboard22SetMovedCursorCallbackEPFvPKNS0_14MovedCursorArgEE" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard23SetDecidedEnterCallbackEPFvPKNS0_15DecidedEnterArgEE" => set_enter,
    "_ZN2nn5swkbd14InlineKeyboard24SetChangedStringCallbackEPFvPKNS0_16ChangedStringArgEE" => set_changed,
    "_ZN2nn5swkbd14InlineKeyboard27SetFinishedKeyboardCallbackEPFvvE" => set_finished,
    "_ZN2nn5swkbd14InlineKeyboard6AppearERKNS0_9AppearArgE" => inline_appear,
    "_ZN2nn5swkbd14InlineKeyboard9DisappearEv" => inline_disappear,
    "_ZN2nn5swkbd14InlineKeyboard4CalcEv" => inline_calc,
    "_ZN2nn5swkbd14InlineKeyboard29SetFinishedInitializeCallbackEPFvvE" => set_fin_init,
    "_ZN2nn5swkbd14InlineKeyboard31SetReleasedUserWordInfoCallbackEPFvvE" => set_released,
    "_ZN2nn5swkbd14InlineKeyboard15SetUserWordInfoERKNS0_12UserWordInfoE" => zero,
    "_ZN2nn5swkbd14InlineKeyboard17UnsetUserWordInfoEv" => inline_unset_words,
    "_ZN2nn5swkbd14InlineKeyboard25SetCustomizedDictionariesERKNS0_23CustomizedDictionarySetE" => zero,
    "_ZN2nn5swkbd14InlineKeyboard27UnsetCustomizedDictionariesEv" => zero,
    "_ZN2nn5swkbd14InlineKeyboard13SetWindowModeENS1_10WindowModeE" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard14SetFooterAlphaEf" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard14SetKeytopAlphaEf" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard14SetKeytopScaleEf" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard18SetKeytopTranslateEff" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard15SetTouchEnabledEb" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard26SetHardwareKeyboardEnabledEb" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard26IsUsedTouchPointByKeyboardEii" => zero,
    "_ZN2nn5swkbd14InlineKeyboard12GetMaxHeightEv" => max_height,
    "_ZN2nn5swkbd14InlineKeyboard12SetInputTextERKNS0_9InputTextE" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard12SetCursorPosEi" => nothing,
    "_ZN2nn5swkbd14InlineKeyboard8GetImageEPvm" => zero,
    "_ZNK2nn5swkbd14InlineKeyboard13GetWindowSizeEPiS2_" => window_size,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_to_utf8_keeps_whole_characters() {
        let units: Vec<u16> = "a\u{e9}\u{1f600}".encode_utf16().collect();
        assert_eq!(utf16_to_utf8(&units, 100), "a\u{e9}\u{1f600}".as_bytes());
        assert_eq!(utf16_to_utf8(&units, 4), b"a\xc3\xa9");
    }

    #[test]
    fn to_utf16_does_not_split_a_surrogate_pair() {
        assert_eq!(to_utf16("a\u{1f600}", 2), vec![u16::from(b'a')]);
        assert_eq!(to_utf16("a\u{1f600}", 3).len(), 3);
    }

    #[test]
    fn a_fourth_inline_keyboard_has_a_slot_and_a_fifth_has_none() {
        let mut slots = [Slot::default(); 4];
        for kb in 1..=4 {
            assert!(slot_index(&mut slots, kb).is_some());
        }
        assert!(slot_index(&mut slots, 5).is_none());
        assert_eq!(slot_index(&mut slots, 3), Some(2));
    }
}
