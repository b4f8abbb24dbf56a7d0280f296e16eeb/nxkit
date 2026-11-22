//! The software keyboard of the console (`nn::swkbd`), as the host's own text input screen.
//!
//! A game thread that shows the keyboard asks [`ask`] and waits. The display tick of the window (`sys/textinputui.rs`)
//! takes the request, shows the screen over the game and answers with the text or a cancel. Without a window that can
//! show it (no overlay, a test without AppKit), [`ask`] says so and the caller stops the game, as before.
//!
//! The lengths of the request count UTF-16 units, as the console's keyboard does ([`units`], [`clamp`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};

pub use nxkit_svc_system::TextRequest;

/// A request between the game thread and the screen.
enum Slot {
    /// Nothing asked.
    Idle,
    /// Asked, not yet shown.
    Asked(TextRequest),
    /// The screen shows it.
    Shown,
    /// Answered: the text, or `None` for a cancel.
    Answered(Option<String>),
}

static SLOT: Mutex<Slot> = Mutex::new(Slot::Idle);
static ANSWERED: Condvar = Condvar::new();
static SCREEN: AtomicBool = AtomicBool::new(false);

/// The window can show the screen (`sys/textinputui.rs` installed it).
pub fn set_screen(on: bool) {
    SCREEN.store(on, Ordering::Release);
}

/// Shows the keyboard of `req` and waits for the player: the text, or `None` for a cancel. `Err` when no window can
/// show it.
pub fn ask(req: &TextRequest) -> Result<Option<String>, &'static str> {
    if !SCREEN.load(Ordering::Acquire) {
        return Err("the window has no overlay for the text input screen");
    }
    let mut slot = SLOT.lock().unwrap_or_else(PoisonError::into_inner);
    // One keyboard at a time, as on the console.
    while !matches!(*slot, Slot::Idle) {
        slot = ANSWERED.wait(slot).unwrap_or_else(PoisonError::into_inner);
    }
    *slot = Slot::Asked(req.clone());
    loop {
        if let Slot::Answered(_) = *slot {
            let Slot::Answered(a) = std::mem::replace(&mut *slot, Slot::Idle) else {
                unreachable!()
            };
            ANSWERED.notify_all();
            return Ok(a);
        }
        slot = ANSWERED.wait(slot).unwrap_or_else(PoisonError::into_inner);
    }
}

/// The request to show, once: the screen takes it.
pub fn take() -> Option<TextRequest> {
    let mut slot = SLOT.lock().unwrap_or_else(PoisonError::into_inner);
    if !matches!(*slot, Slot::Asked(_)) {
        return None;
    }
    let Slot::Asked(req) = std::mem::replace(&mut *slot, Slot::Shown) else {
        unreachable!()
    };
    Some(req)
}

/// The screen's answer: the text, or `None` for a cancel.
pub fn answer(text: Option<String>) {
    let mut slot = SLOT.lock().unwrap_or_else(PoisonError::into_inner);
    if matches!(*slot, Slot::Shown) {
        *slot = Slot::Answered(text);
        ANSWERED.notify_all();
    }
}

/// The length of `text` in UTF-16 units.
pub fn units(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// `text` cut to at most `max` UTF-16 units, at a character boundary. `max` 0 means no limit.
pub fn clamp(text: &str, max: u32) -> &str {
    if max == 0 {
        return text;
    }
    let mut used = 0u32;
    for (at, c) in text.char_indices() {
        used += c.len_utf16() as u32;
        if used > max {
            return &text[..at];
        }
    }
    text
}

/// Whether `text` may be sent: at least `min` and at most `max` UTF-16 units (`max` 0: no limit).
pub fn acceptable(text: &str, min: u32, max: u32) -> bool {
    let n = units(text);
    n >= min && (max == 0 || n <= max)
}

/// A key of the on-screen keyboard that a controller types with, or an action of the screen in its last row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character, upper case while shift is on.
    Char(char),
    /// Shift for the next character.
    Shift,
    /// A space.
    Space,
    /// Deletes the character before the caret.
    Back,
    /// Cancels the screen.
    Cancel,
    /// Sends the text.
    Ok,
}

/// A row of keys with their widths in key units. Every row is [`COLUMNS`] units wide.
pub type Row = Vec<(Key, u8)>;

/// The width of every row in key units.
pub const COLUMNS: u8 = 11;

/// The rows of the on-screen keyboard: QWERTZ with the umlauts for `german`, else QWERTY, then shift, space and
/// delete, then Cancel and OK.
pub fn rows(german: bool) -> Vec<Row> {
    let chars = |s: &str| -> Row { s.chars().map(|c| (Key::Char(c), 1)).collect() };
    let letters: [&str; 4] = if german {
        ["1234567890-", "qwertzuiopü", "asdfghjklöä", "yxcvbnm,.?!"]
    } else {
        ["1234567890-", "qwertyuiop@", "asdfghjkl:'", "zxcvbnm,.?!"]
    };
    let mut rows: Vec<Row> = letters.iter().map(|s| chars(s)).collect();
    rows.push(vec![(Key::Shift, 2), (Key::Space, 7), (Key::Back, 2)]);
    rows.push(vec![(Key::Cancel, 5), (Key::Ok, 6)]);
    rows
}

/// A direction of the d-pad or the stick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Up.
    Up,
    /// Down.
    Down,
    /// Left.
    Left,
    /// Right.
    Right,
}

/// The focus after a move from `(row, key)`. Left and right stop at the ends of the row. Up and down take the key of
/// the next row under the middle of this one, so a wide key leads to the key under its middle.
pub fn step(rows: &[Row], (row, key): (usize, usize), dir: Dir) -> (usize, usize) {
    let middle = |r: usize, k: usize| -> f32 {
        let before: u32 = rows[r][..k].iter().map(|(_, w)| u32::from(*w)).sum();
        before as f32 + f32::from(rows[r][k].1) / 2.0
    };
    let at = |r: usize, x: f32| -> usize {
        let mut left = 0.0;
        for (k, (_, w)) in rows[r].iter().enumerate() {
            let right = left + f32::from(*w);
            if x < right {
                return k;
            }
            left = right;
        }
        rows[r].len() - 1
    };
    match dir {
        Dir::Left => (row, key.saturating_sub(1)),
        Dir::Right => (row, (key + 1).min(rows[row].len() - 1)),
        Dir::Up if row > 0 => (row - 1, at(row - 1, middle(row, key))),
        Dir::Down if row + 1 < rows.len() => (row + 1, at(row + 1, middle(row, key))),
        _ => (row, key),
    }
}

/// Where a key is: its row and index.
pub fn find(rows: &[Row], wanted: Key) -> Option<(usize, usize)> {
    rows.iter()
        .enumerate()
        .find_map(|(r, row)| row.iter().position(|(k, _)| *k == wanted).map(|i| (r, i)))
}

/// `text` after typing `c`, upper case with `shift`, within `max` UTF-16 units (0: no limit). `None` when it would
/// not fit.
pub fn typed(text: &str, c: char, shift: bool, max: u32) -> Option<String> {
    let mut t = text.to_owned();
    if shift {
        t.extend(c.to_uppercase());
    } else {
        t.push(c);
    }
    (max == 0 || units(&t) <= max).then_some(t)
}

/// `text` without its last character.
pub fn deleted(text: &str) -> String {
    let mut t = text.to_owned();
    t.pop();
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_count_utf16_units() {
        assert_eq!(units("Zijad"), 5);
        assert_eq!(units("ü"), 1);
        // An emoji outside the basic plane takes two units.
        assert_eq!(units("a😀"), 3);
    }

    #[test]
    fn clamp_cuts_at_a_character() {
        assert_eq!(clamp("Pokopia", 4), "Poko");
        assert_eq!(clamp("Pokopia", 0), "Pokopia");
        assert_eq!(clamp("ab😀", 3), "ab");
        assert_eq!(clamp("ab😀", 4), "ab😀");
    }

    #[test]
    fn acceptable_follows_the_bounds() {
        assert!(!acceptable("", 1, 10));
        assert!(acceptable("A", 1, 10));
        assert!(acceptable("", 0, 10));
        assert!(!acceptable("ABCDEFGHIJK", 1, 10));
    }

    #[test]
    fn every_row_is_as_wide_as_the_keyboard() {
        for german in [true, false] {
            for row in rows(german) {
                assert_eq!(row.iter().map(|(_, w)| w).sum::<u8>(), COLUMNS);
            }
        }
    }

    #[test]
    fn the_focus_moves_by_the_middle_of_the_keys() {
        let r = rows(true);
        // From `n` (row 3, key 5) down to space, which lies under it.
        assert_eq!(step(&r, (3, 5), Dir::Down), (4, 1));
        // From space down to OK, which holds the middle of the space bar (5.5 of 11 units)
        assert_eq!(step(&r, (4, 1), Dir::Down), (5, 1));
        // From Cancel up to the space bar, and from the space bar up to the key under its middle.
        assert_eq!(step(&r, (5, 0), Dir::Up), (4, 1));
        assert_eq!(step(&r, (4, 1), Dir::Up), (3, 5));
        // The ends stop.
        assert_eq!(step(&r, (0, 0), Dir::Left), (0, 0));
        assert_eq!(step(&r, (0, 10), Dir::Right), (0, 10));
        assert_eq!(step(&r, (0, 3), Dir::Up), (0, 3));
        assert_eq!(find(&r, Key::Ok), Some((5, 1)));
    }

    #[test]
    fn typing_follows_shift_and_the_limit() {
        assert_eq!(typed("", 'z', true, 10).as_deref(), Some("Z"));
        assert_eq!(typed("Z", 'i', false, 10).as_deref(), Some("Zi"));
        assert_eq!(typed("ABC", 'd', false, 3), None);
        assert_eq!(deleted("Zijad"), "Zija");
        assert_eq!(deleted(""), "");
    }

    #[test]
    fn a_request_is_answered_once() {
        set_screen(true);
        let req = TextRequest {
            header: "Name".into(),
            sub: String::new(),
            guide: String::new(),
            initial: String::new(),
            max_len: 10,
            min_len: 1,
        };
        let asker = std::thread::spawn(move || ask(&req));
        let shown = loop {
            if let Some(r) = take() {
                break r;
            }
            std::thread::yield_now();
        };
        assert_eq!(shown.header, "Name");
        assert!(take().is_none(), "the screen takes a request once");
        answer(Some("Zijad".into()));
        assert_eq!(asker.join().unwrap(), Ok(Some("Zijad".into())));
        set_screen(false);
    }
}
