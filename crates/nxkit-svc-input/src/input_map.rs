//! The button maps of the launcher: `NXKIT_KEYMAP` puts a function on another key, `NXKIT_PAD_MAP` changes what the
//! buttons of a controller send. The values have the format of `DM_KEYMAP` and `DM_PAD_MAP` of the old engine
//! (`dm/host/input_map.h`), so a launcher passes the same text to both engines.
//!
//! Both are lists of `<name>=<value>` entries, separated by commas. A trailing comma is allowed. The names are
//! case-insensitive: the 16 buttons `A B X Y LS RS L R ZL ZR Plus Minus Left Up Right Down`, the four directions of
//! each stick `LUp LDown LLeft LRight RUp RDown RLeft RRight` and `Shake`.
//!
//! A key map entry is `<function>=<key code>`. The key code is a macOS virtual key code (`NSEvent.keyCode`, decimal,
//! below 128) or `none`. The function leaves every key that held it and goes to that key, and the old function of the
//! key leaves it, so `A=40,B=38` swaps A and B. It is applied on top of the PC layout of `bind`. Mouse buttons and the
//! wheel keep their functions.
//!
//! A pad map entry is `<button>=<button>`: the button that sends the first one by default (by its position) sends the
//! second one instead, or nothing for `none`. Buttons that are not named stay, so `A=B,B=A` swaps them. Only the 16
//! buttons are sources, and the sticks stay sticks.
//!
//! nxkit has no shake. A `Shake` entry is taken so that a value of the old engine parses whole, and it changes
//! nothing in a key map. A button mapped to `Shake` sends nothing.
//!
//! A value is taken whole or not at all, as `input_map_valid()` of the old engine checks it: an empty value or one
//! malformed entry gives a [`MapError`].

use std::fmt;

use crate::bind::{Action, Bindings, Dir};
use crate::mac_keycodes;

const BUTTONS: usize = 16;
const SHAKE: usize = 24;
/// Key codes are below this.
const KEY_CODES: u32 = 128;

const NAMES: [&str; 25] = [
    "A", "B", "X", "Y", "LS", "RS", "L", "R", "ZL", "ZR", "Plus", "Minus", "Left", "Up", "Right",
    "Down", "LUp", "LDown", "LLeft", "LRight", "RUp", "RDown", "RLeft", "RRight", "Shake",
];

/// A value that is not taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapError {
    /// The entries that are not `<name>=<value>`. Empty when the whole value is empty.
    pub bad: Vec<String>,
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.bad.is_empty() {
            return f.write_str("the value is empty");
        }
        let list: Vec<String> = self.bad.iter().map(|e| format!("'{e}'")).collect();
        write!(f, "{} is not <name>=<value>", list.join(", "))
    }
}

impl std::error::Error for MapError {}

/// The function of a name, as an index into [`NAMES`]. Case-insensitive.
fn function(name: &str) -> Option<usize> {
    NAMES.iter().position(|n| n.eq_ignore_ascii_case(name))
}

/// The entries of `spec`, each parsed by `one` from the function of its name and its value. `max` is the first
/// function that is no source. Every malformed entry is collected.
fn walk<T>(
    spec: &str,
    max: usize,
    mut one: impl FnMut(usize, &str) -> Option<T>,
) -> Result<Vec<T>, MapError> {
    if spec.is_empty() {
        return Err(MapError { bad: Vec::new() });
    }
    let mut parts: Vec<&str> = spec.split(',').collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for entry in parts {
        let parsed = entry.split_once('=').and_then(|(name, value)| {
            function(name)
                .filter(|f| *f < max)
                .and_then(|f| one(f, value))
        });
        match parsed {
            Some(v) => out.push(v),
            None => bad.push(entry.to_owned()),
        }
    }
    if bad.is_empty() {
        Ok(out)
    } else {
        Err(MapError { bad })
    }
}

/// The action of a function in the layout of `bind`. `None` for Shake.
fn action(f: usize) -> Option<Action> {
    const DIRS: [Dir; 4] = [Dir::Up, Dir::Down, Dir::Left, Dir::Right];
    match f {
        0..BUTTONS => Some(Action::Button(1 << f)),
        16..20 => Some(Action::LeftStick(DIRS[f - 16])),
        20..SHAKE => Some(Action::RightStick(DIRS[f - 20])),
        _ => None,
    }
}

/// The changes of `NXKIT_KEYMAP` to the keyboard layout of player 1, in the order of the value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyMap {
    /// The function and its key code, `None` for no key.
    entries: Vec<(usize, Option<u8>)>,
}

impl KeyMap {
    /// Parses a value of `NXKIT_KEYMAP` (`DM_KEYMAP`).
    pub fn parse(spec: &str) -> Result<KeyMap, MapError> {
        let entries = walk(spec, NAMES.len(), |f, v| {
            if v.eq_ignore_ascii_case("none") {
                return Some((f, None));
            }
            if v.is_empty() || v.len() >= 8 || !v.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let code: u32 = v.parse().ok()?;
            (code < KEY_CODES).then_some((f, Some(code as u8)))
        })?;
        Ok(KeyMap { entries })
    }

    /// The key codes of the value that name a key the platform layer does not read. Their functions are on no key.
    pub fn unread_codes(&self) -> Vec<u8> {
        self.entries
            .iter()
            .filter(|(f, _)| *f != SHAKE)
            .filter_map(|(_, code)| code.filter(|c| mac_keycodes::usage_of(*c).is_none()))
            .collect()
    }

    /// Moves the functions of the entries in `b`, in order.
    pub(crate) fn apply(&self, b: &mut Bindings) {
        for &(f, code) in &self.entries {
            let Some(a) = action(f) else { continue };
            b.unbind_keys_of(a);
            if let Some(key) = code.and_then(mac_keycodes::usage_of) {
                b.bind_key(key, a);
            }
        }
    }
}

/// What a button of a controller sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The `NpadButton` bit number.
    Button(u8),
    /// The shake of the old engine. nxkit has none, so the button sends nothing.
    Shake,
    Nothing,
}

/// What the buttons of a controller send (`NXKIT_PAD_MAP`), by the button that they send by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadMap {
    to: [Target; BUTTONS],
}

impl Default for PadMap {
    fn default() -> Self {
        PadMap {
            to: std::array::from_fn(|b| Target::Button(b as u8)),
        }
    }
}

impl PadMap {
    /// Parses a value of `NXKIT_PAD_MAP` (`DM_PAD_MAP`), on top of the default where every button sends itself.
    pub fn parse(spec: &str) -> Result<PadMap, MapError> {
        let entries = walk(spec, BUTTONS, |f, v| {
            if v.eq_ignore_ascii_case("none") {
                return Some((f, Target::Nothing));
            }
            match function(v)? {
                SHAKE => Some((f, Target::Shake)),
                t if t < BUTTONS => Some((f, Target::Button(t as u8))),
                _ => None,
            }
        })?;
        let mut map = PadMap::default();
        for (f, t) in entries {
            map.to[f] = t;
        }
        Ok(map)
    }

    /// True when every button sends itself.
    pub fn is_default(&self) -> bool {
        *self == PadMap::default()
    }

    /// The `NpadButton` bits that a controller sends for the bits it reads by default. Bits above the 16 buttons
    /// pass.
    pub(crate) fn apply(&self, buttons: u64) -> u64 {
        let mut out = buttons & !0xffff;
        for (b, t) in self.to.iter().enumerate() {
            if buttons >> b & 1 != 0
                && let Target::Button(to) = t
            {
                out |= 1 << to;
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "input_map_tests.rs"]
mod tests;
