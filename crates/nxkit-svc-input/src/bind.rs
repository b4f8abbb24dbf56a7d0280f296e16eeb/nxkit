//! The keyboard and mouse layout of player 1: which key or mouse button presses which Switch button or moves which
//! stick (R63).
//!
//! [`Bindings::pc`] is the PC game layout that every title gets. A title may change single bindings with the
//! `bind` table of its input file. The host reads the file and hands the pairs over as text, [`Bindings::rebind`]
//! parses the names here. The camera stick of the mouse is in `look`.

use crate::keys::{KeySet, usage};
use crate::npad::{RawKeys, btn};

/// The full deflection of a stick.
const FULL: i32 = 32767;
/// The deflection of the left stick while the walk key is held.
const WALK: i32 = FULL / 2;

/// Mouse button bits, as the host reports them. They are also the `MouseButton` bits of `nn::hid`.
pub(crate) mod mouse {
    pub(crate) const LEFT: u32 = 1 << 0;
    pub(crate) const RIGHT: u32 = 1 << 1;
    pub(crate) const MIDDLE: u32 = 1 << 2;
    pub(crate) const FORWARD: u32 = 1 << 3;
    pub(crate) const BACK: u32 = 1 << 4;
}

/// What can be bound: a key, a mouse button or a direction of the wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// A key by its USB usage id.
    Key(u8),
    /// A mouse button, one bit of [`mouse`].
    Mouse(u32),
    /// The wheel turned up, as a short press.
    WheelUp,
    /// The wheel turned down, as a short press.
    WheelDown,
}

/// A direction of a stick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// What a source does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Presses `NpadButton` bits.
    Button(u64),
    /// Pushes the left stick fully in a direction.
    LeftStick(Dir),
    /// Pushes the right stick fully in a direction.
    RightStick(Dir),
    /// Halves the deflection of the left stick, for walking.
    Walk,
}

/// What the mouse does at one moment: the held buttons and the wheel presses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MouseButtons {
    /// [`mouse`] bits.
    pub(crate) buttons: u32,
    /// The wheel turned up within the press time.
    pub(crate) wheel_up: bool,
    /// The wheel turned down within the press time.
    pub(crate) wheel_down: bool,
}

/// The sources of a key or mouse name. A name like `shift` stands for both shift keys. `None` for an unknown name.
/// Names are case-insensitive.
pub(crate) fn sources(name: &str) -> Option<Vec<Source>> {
    use Source::{Key, Mouse};
    let name = name.trim().to_ascii_lowercase();
    let one = |s: Source| Some(vec![s]);
    let b = name.as_bytes();
    if b.len() == 1 {
        return match b[0] {
            c @ b'a'..=b'z' => one(Key(usage::letter(c))),
            b'0' => one(Key(usage::ZERO)),
            c @ b'1'..=b'9' => one(Key(usage::ONE + (c - b'1'))),
            _ => None,
        };
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=12).contains(&n).then(|| vec![Key(usage::F1 + n - 1)]);
    }
    let keys: &[u8] = match name.as_str() {
        "space" => &[usage::SPACE],
        "enter" | "return" => &[usage::RETURN],
        "tab" => &[usage::TAB],
        "backspace" => &[usage::BACKSPACE],
        "delete" => &[usage::DELETE_FORWARD],
        "escape" | "esc" => &[usage::ESCAPE],
        "up" => &[usage::UP],
        "down" => &[usage::DOWN],
        "left" => &[usage::LEFT],
        "right" => &[usage::RIGHT],
        "shift" => &[usage::LEFT_SHIFT, usage::RIGHT_SHIFT],
        "lshift" => &[usage::LEFT_SHIFT],
        "rshift" => &[usage::RIGHT_SHIFT],
        "ctrl" | "control" => &[usage::LEFT_CONTROL, usage::RIGHT_CONTROL],
        "lctrl" => &[usage::LEFT_CONTROL],
        "rctrl" => &[usage::RIGHT_CONTROL],
        "alt" | "option" => &[usage::LEFT_ALT, usage::RIGHT_ALT],
        "lalt" => &[usage::LEFT_ALT],
        "ralt" => &[usage::RIGHT_ALT],
        "capslock" => &[usage::CAPS_LOCK],
        "minus" | "-" => &[usage::HYPHEN],
        "equal" | "=" => &[usage::EQUAL],
        "lbracket" => &[usage::OPEN_BRACKET],
        "rbracket" => &[usage::CLOSE_BRACKET],
        "backslash" => &[usage::BACKSLASH],
        "semicolon" => &[usage::SEMICOLON],
        "quote" => &[usage::QUOTE],
        "grave" => &[usage::GRAVE],
        "comma" => &[usage::COMMA],
        "period" => &[usage::PERIOD],
        "slash" => &[usage::SLASH],
        _ => &[],
    };
    if !keys.is_empty() {
        return Some(keys.iter().map(|&k| Key(k)).collect());
    }
    match name.as_str() {
        "mouse_left" | "mouse1" => one(Mouse(mouse::LEFT)),
        "mouse_right" | "mouse2" => one(Mouse(mouse::RIGHT)),
        "mouse_middle" | "mouse3" => one(Mouse(mouse::MIDDLE)),
        "mouse4" => one(Mouse(mouse::BACK)),
        "mouse5" => one(Mouse(mouse::FORWARD)),
        "wheel" => Some(vec![Source::WheelUp, Source::WheelDown]),
        "wheel_up" => one(Source::WheelUp),
        "wheel_down" => one(Source::WheelDown),
        _ => None,
    }
}

/// The action of a name: a Switch button (`a`, `zr`, `plus`, `dpad_up`, `lstick`), a stick direction (`ls_up`,
/// `rs_left`) or `walk`. `Ok(None)` for `none`, which removes a binding. `Err` for an unknown name.
pub(crate) fn action(name: &str) -> Result<Option<Action>, ()> {
    use Action::{Button, LeftStick, RightStick};
    let name = name.trim().to_ascii_lowercase();
    let a = match name.as_str() {
        "none" | "" => return Ok(None),
        "a" => Button(btn::A),
        "b" => Button(btn::B),
        "x" => Button(btn::X),
        "y" => Button(btn::Y),
        "l" => Button(btn::L),
        "r" => Button(btn::R),
        "zl" => Button(btn::ZL),
        "zr" => Button(btn::ZR),
        "plus" | "+" => Button(btn::PLUS),
        "minus" | "-" => Button(btn::MINUS),
        "lstick" | "ls" => Button(btn::STICK_L),
        "rstick" | "rs" => Button(btn::STICK_R),
        "dpad_up" => Button(btn::UP),
        "dpad_down" => Button(btn::DOWN),
        "dpad_left" => Button(btn::LEFT),
        "dpad_right" => Button(btn::RIGHT),
        "ls_up" => LeftStick(Dir::Up),
        "ls_down" => LeftStick(Dir::Down),
        "ls_left" => LeftStick(Dir::Left),
        "ls_right" => LeftStick(Dir::Right),
        "rs_up" => RightStick(Dir::Up),
        "rs_down" => RightStick(Dir::Down),
        "rs_left" => RightStick(Dir::Left),
        "rs_right" => RightStick(Dir::Right),
        "walk" => Action::Walk,
        _ => return Err(()),
    };
    Ok(Some(a))
}

/// The bindings of player 1. A source has at most one action. Several sources may share an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bindings {
    list: Vec<(Source, Action)>,
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings::pc()
    }
}

impl Bindings {
    /// The PC layout (R63). W A S D walk, the mouse is the camera, E interacts, Space jumps, Shift runs, the left
    /// mouse button attacks and the right one locks on. The the engine keys that do not collide stay: J K U I are A B X Y,
    /// G is the right stick click, Z is ZL, Return is Plus and the arrows are the D-pad.
    pub(crate) fn pc() -> Bindings {
        use Action::{Button, LeftStick, Walk};
        let named: &[(&str, Action)] = &[
            ("w", LeftStick(Dir::Up)),
            ("s", LeftStick(Dir::Down)),
            ("a", LeftStick(Dir::Left)),
            ("d", LeftStick(Dir::Right)),
            ("alt", Walk),
            ("e", Button(btn::A)),
            ("space", Button(btn::X)),
            ("shift", Button(btn::B)),
            ("mouse_left", Button(btn::Y)),
            ("mouse_right", Button(btn::ZL)),
            ("q", Button(btn::L)),
            ("r", Button(btn::R)),
            ("wheel", Button(btn::ZR)),
            ("f", Button(btn::ZR)),
            ("ctrl", Button(btn::STICK_L)),
            ("c", Button(btn::STICK_L)),
            ("mouse_middle", Button(btn::STICK_R)),
            ("tab", Button(btn::PLUS)),
            ("m", Button(btn::MINUS)),
            ("1", Button(btn::UP)),
            ("2", Button(btn::RIGHT)),
            ("3", Button(btn::DOWN)),
            ("4", Button(btn::LEFT)),
            ("up", Button(btn::UP)),
            ("right", Button(btn::RIGHT)),
            ("down", Button(btn::DOWN)),
            ("left", Button(btn::LEFT)),
            ("enter", Button(btn::PLUS)),
            ("j", Button(btn::A)),
            ("k", Button(btn::B)),
            ("u", Button(btn::X)),
            ("i", Button(btn::Y)),
            ("g", Button(btn::STICK_R)),
            ("z", Button(btn::ZL)),
        ];
        let mut list = Vec::new();
        for (name, a) in named {
            for s in sources(name).unwrap_or_default() {
                list.push((s, *a));
            }
        }
        Bindings { list }
    }

    /// The action of a source.
    #[cfg(test)]
    pub(crate) fn get(&self, source: Source) -> Option<Action> {
        self.list
            .iter()
            .find(|(s, _)| *s == source)
            .map(|(_, a)| *a)
    }

    /// Binds the sources of `key` to `action`, or unbinds them for `none`. The old action of each source goes.
    pub(crate) fn rebind(&mut self, key: &str, action_name: &str) -> Result<(), String> {
        let sources =
            sources(key).ok_or_else(|| format!("unknown key or mouse button \"{key}\""))?;
        let action = action(action_name)
            .map_err(|()| format!("unknown action \"{action_name}\" for \"{key}\""))?;
        self.list.retain(|(s, _)| !sources.contains(s));
        if let Some(a) = action {
            self.list.extend(sources.into_iter().map(|s| (s, a)));
        }
        Ok(())
    }

    /// Applies the pairs of an input file in order. Each pair that does not parse is skipped and its message is
    /// returned.
    pub(crate) fn rebind_all(&mut self, pairs: &[(String, String)]) -> Vec<String> {
        pairs
            .iter()
            .filter_map(|(k, a)| self.rebind(k, a).err())
            .collect()
    }

    /// The buttons and the stick deflections of the held keys and mouse buttons. Opposite directions cancel.
    pub(crate) fn read(&self, keys: &KeySet, mouse: MouseButtons) -> RawKeys {
        let mut buttons = 0;
        let mut walk = false;
        // Left up, down, left, right, then the same for the right stick.
        let mut dirs = [false; 8];
        for &(source, action) in &self.list {
            let held = match source {
                Source::Key(k) => keys.get(k),
                Source::Mouse(m) => mouse.buttons & m != 0,
                Source::WheelUp => mouse.wheel_up,
                Source::WheelDown => mouse.wheel_down,
            };
            if !held {
                continue;
            }
            let dir_index = |d: Dir| match d {
                Dir::Up => 0,
                Dir::Down => 1,
                Dir::Left => 2,
                Dir::Right => 3,
            };
            match action {
                Action::Button(b) => buttons |= b,
                Action::LeftStick(d) => dirs[dir_index(d)] = true,
                Action::RightStick(d) => dirs[4 + dir_index(d)] = true,
                Action::Walk => walk = true,
            }
        }
        let axis = |neg: bool, pos: bool, full: i32| match (neg, pos) {
            (true, false) => -full,
            (false, true) => full,
            _ => 0,
        };
        let left = if walk { WALK } else { FULL };
        RawKeys {
            buttons,
            sticks: [
                axis(dirs[2], dirs[3], left),
                axis(dirs[1], dirs[0], left),
                axis(dirs[6], dirs[7], FULL),
                axis(dirs[5], dirs[4], FULL),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> KeySet {
        let mut k = KeySet::default();
        for n in names {
            for s in sources(n).unwrap_or_default() {
                if let Source::Key(u) = s {
                    k.set(u);
                }
            }
        }
        k
    }

    fn pc(names: &[&str]) -> RawKeys {
        Bindings::pc().read(&keys(names), MouseButtons::default())
    }

    #[test]
    fn names_have_their_usage_ids() {
        assert_eq!(sources("A"), Some(vec![Source::Key(0x04)]));
        assert_eq!(sources("1"), Some(vec![Source::Key(0x1e)]));
        assert_eq!(sources("0"), Some(vec![Source::Key(0x27)]));
        assert_eq!(sources("f12"), Some(vec![Source::Key(0x45)]));
        assert_eq!(sources("f13"), None);
        assert_eq!(
            sources("shift"),
            Some(vec![Source::Key(0xe1), Source::Key(0xe5)])
        );
        assert_eq!(
            sources("wheel"),
            Some(vec![Source::WheelUp, Source::WheelDown])
        );
        assert_eq!(
            sources("mouse_middle"),
            Some(vec![Source::Mouse(mouse::MIDDLE)])
        );
        assert_eq!(sources("hyper"), None);
    }

    #[test]
    fn action_names() {
        assert_eq!(action("ZR"), Ok(Some(Action::Button(btn::ZR))));
        assert_eq!(action("ls_up"), Ok(Some(Action::LeftStick(Dir::Up))));
        assert_eq!(action("none"), Ok(None));
        assert_eq!(action("jump"), Err(()));
    }

    #[test]
    fn wasd_is_the_left_stick_at_full_deflection() {
        assert_eq!(pc(&["w", "a"]).sticks, [-FULL, FULL, 0, 0]);
        assert_eq!(pc(&["s", "d"]).sticks, [FULL, -FULL, 0, 0]);
        assert_eq!(pc(&["w", "s"]).sticks, [0; 4]);
        assert_eq!(pc(&["w", "s"]).buttons, 0);
    }

    #[test]
    fn alt_walks_at_half_deflection() {
        assert_eq!(pc(&["w", "alt"]).sticks, [0, WALK, 0, 0]);
        assert_eq!(pc(&["alt"]).sticks, [0; 4]);
    }

    #[test]
    fn the_pc_keys() {
        let cases: &[(&str, u64)] = &[
            ("e", btn::A),
            ("space", btn::X),
            ("lshift", btn::B),
            ("rshift", btn::B),
            ("q", btn::L),
            ("r", btn::R),
            ("f", btn::ZR),
            ("lctrl", btn::STICK_L),
            ("c", btn::STICK_L),
            ("tab", btn::PLUS),
            ("enter", btn::PLUS),
            ("m", btn::MINUS),
            ("1", btn::UP),
            ("2", btn::RIGHT),
            ("3", btn::DOWN),
            ("4", btn::LEFT),
            ("up", btn::UP),
            ("left", btn::LEFT),
            ("j", btn::A),
            ("k", btn::B),
            ("u", btn::X),
            ("i", btn::Y),
            ("g", btn::STICK_R),
            ("z", btn::ZL),
        ];
        for &(name, b) in cases {
            assert_eq!(pc(&[name]).buttons, b, "{name}");
        }
    }

    #[test]
    fn the_mouse_buttons_and_the_wheel() {
        let b = Bindings::pc();
        let read = |buttons, wheel_up, wheel_down| {
            b.read(
                &KeySet::default(),
                MouseButtons {
                    buttons,
                    wheel_up,
                    wheel_down,
                },
            )
            .buttons
        };
        assert_eq!(read(mouse::LEFT, false, false), btn::Y);
        assert_eq!(read(mouse::RIGHT, false, false), btn::ZL);
        assert_eq!(read(mouse::MIDDLE, false, false), btn::STICK_R);
        assert_eq!(read(0, true, false), btn::ZR);
        assert_eq!(read(0, false, true), btn::ZR);
        assert_eq!(read(mouse::BACK, false, false), 0);
    }

    #[test]
    fn rebinding_replaces_the_old_action() {
        let mut b = Bindings::pc();
        assert!(b.rebind("F", "rstick").is_ok());
        assert_eq!(
            b.get(Source::Key(usage::letter(b'f'))),
            Some(Action::Button(btn::STICK_R))
        );
        assert!(b.rebind("mouse4", "ls_up").is_ok());
        assert_eq!(
            b.read(
                &KeySet::default(),
                MouseButtons {
                    buttons: mouse::BACK,
                    ..Default::default()
                }
            )
            .sticks[1],
            FULL
        );
        assert!(b.rebind("shift", "none").is_ok());
        assert_eq!(b.get(Source::Key(usage::LEFT_SHIFT)), None);
        assert_eq!(b.get(Source::Key(usage::RIGHT_SHIFT)), None);
    }

    #[test]
    fn bad_pairs_are_skipped_with_a_message() {
        let mut b = Bindings::pc();
        let errors = b.rebind_all(&[
            ("hyper".to_owned(), "a".to_owned()),
            ("x".to_owned(), "jump".to_owned()),
            ("x".to_owned(), "y".to_owned()),
        ]);
        assert_eq!(errors.len(), 2);
        assert_eq!(
            b.get(Source::Key(usage::letter(b'x'))),
            Some(Action::Button(btn::Y))
        );
    }

    #[test]
    fn right_stick_keys_can_be_bound() {
        let mut b = Bindings::pc();
        assert!(b.rebind("left", "rs_left").is_ok());
        assert!(b.rebind("up", "rs_up").is_ok());
        let k = b.read(&keys(&["left", "up"]), MouseButtons::default());
        assert_eq!(k.sticks, [0, 0, -FULL, FULL]);
        assert_eq!(k.buttons, 0);
    }
}
