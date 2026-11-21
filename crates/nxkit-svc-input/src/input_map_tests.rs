//! The cases of `dm/host/tests/t_input_map.c` of the old engine. The key map cases run against the PC layout of
//! `bind`, not against the layout of the old engine, so the keys that a moved function leaves are those of R63. The
//! cases of the old engine's default layout and of Shift with the arrows have no counterpart: that layout is not
//! nxkit's.

use super::*;
use crate::bind::Source;
use crate::keys::usage;
use crate::npad::btn;

fn on(b: &Bindings, code: u8) -> Option<Action> {
    b.get(Source::Key(mac_keycodes::usage_of(code)?))
}

fn keyed(spec: &str) -> Bindings {
    let mut b = Bindings::pc();
    KeyMap::parse(spec).unwrap().apply(&mut b);
    b
}

fn keys_with(b: &Bindings, a: Action) -> usize {
    (0..=255u8)
        .filter(|u| b.get(Source::Key(*u)) == Some(a))
        .count()
}

fn pad(spec: &str) -> PadMap {
    PadMap::parse(spec).unwrap()
}

#[test]
fn a_swap_of_two_functions() {
    let b = keyed("A=40,B=38");
    assert_eq!(on(&b, 40), Some(Action::Button(btn::A)));
    assert_eq!(on(&b, 38), Some(Action::Button(btn::B)));
    // A and B leave every other key: E and both shift keys
    assert_eq!(keys_with(&b, Action::Button(btn::A)), 1);
    assert_eq!(keys_with(&b, Action::Button(btn::B)), 1);
}

#[test]
fn names_are_case_insensitive_and_the_key_loses_its_old_function() {
    let b = keyed("a=49");
    assert_eq!(on(&b, 49), Some(Action::Button(btn::A)));
    assert_eq!(on(&b, 38), None, "A leaves J");
    assert_eq!(on(&b, 14), None, "A leaves E");
    assert_eq!(
        keys_with(&b, Action::Button(btn::X)),
        1,
        "X stays on U only"
    );
}

#[test]
fn the_right_stick_on_the_arrows_and_a_function_on_no_key() {
    let b = keyed("RUp=126,RLeft=123,Up=none");
    assert_eq!(on(&b, 126), Some(Action::RightStick(Dir::Up)));
    assert_eq!(on(&b, 123), Some(Action::RightStick(Dir::Left)));
    assert_eq!(keys_with(&b, Action::Button(btn::UP)), 0, "Up=none");
}

#[test]
fn a_function_moves_to_a_key_that_is_not_read() {
    let k = KeyMap::parse("A=65,B=40").unwrap();
    assert_eq!(k.unread_codes(), vec![65]);
    let mut b = Bindings::pc();
    k.apply(&mut b);
    assert_eq!(keys_with(&b, Action::Button(btn::A)), 0);
}

#[test]
fn shake_changes_no_key() {
    let k = KeyMap::parse("Shake=38").unwrap();
    assert!(k.unread_codes().is_empty());
    let mut b = Bindings::pc();
    k.apply(&mut b);
    assert_eq!(b, Bindings::pc());
}

#[test]
fn mouse_buttons_keep_their_functions() {
    let b = keyed("Y=40");
    assert_eq!(on(&b, 40), Some(Action::Button(btn::Y)));
    assert_eq!(
        b.get(Source::Mouse(crate::bind::mouse::LEFT)),
        Some(Action::Button(btn::Y))
    );
    assert_eq!(on(&b, 34), None, "Y leaves I");
}

#[test]
fn five_malformed_entries_are_named_and_nothing_applies() {
    let e = KeyMap::parse("Q=1,ZL=abc,A=128,B=,=5,X=7").unwrap_err();
    assert_eq!(e.bad, ["Q=1", "ZL=abc", "A=128", "B=", "=5"]);
    assert_eq!(
        e.to_string(),
        "'Q=1', 'ZL=abc', 'A=128', 'B=', '=5' is not <name>=<value>"
    );
}

#[test]
fn a_trailing_comma_is_fine_and_an_empty_value_is_not() {
    assert!(KeyMap::parse("L=12,").is_ok());
    assert_eq!(KeyMap::parse("").unwrap_err().bad, Vec::<String>::new());
    assert_eq!(
        KeyMap::parse("").unwrap_err().to_string(),
        "the value is empty"
    );
}

#[test]
fn the_default_pad_map_sends_every_button_itself() {
    let m = PadMap::default();
    assert!(m.is_default());
    let all = btn::A | btn::ZR | btn::STICK_L_UP;
    assert_eq!(m.apply(all), all, "bits above the buttons pass");
}

#[test]
fn a_pad_swap() {
    let m = pad("A=B,B=A");
    assert!(!m.is_default());
    assert_eq!(m.apply(btn::A), btn::B);
    assert_eq!(m.apply(btn::B), btn::A);
    assert_eq!(m.apply(btn::A | btn::B), btn::A | btn::B);
    assert_eq!(
        m.apply(btn::X | btn::DOWN),
        btn::X | btn::DOWN,
        "others stay"
    );
}

#[test]
fn two_buttons_on_one_and_none() {
    let m = pad("ZL=L,minus=none");
    assert_eq!(m.apply(btn::ZL), btn::L);
    assert_eq!(m.apply(btn::L | btn::ZL), btn::L);
    assert_eq!(m.apply(btn::MINUS), 0);
}

#[test]
fn sticks_and_shake_are_no_buttons_of_a_pad_map() {
    let e = PadMap::parse("A=LUp,LUp=A,Shake=B,Y=X").unwrap_err();
    assert_eq!(e.bad, ["A=LUp", "LUp=A", "Shake=B"]);
}

#[test]
fn a_button_on_shake_sends_nothing() {
    let m = pad("A=Shake");
    assert_eq!(m.apply(btn::A | btn::B), btn::B);
}

#[test]
fn valid_values() {
    for v in ["A=40,B=38", "RUp=126,Up=none,Shake=49", "a=49,"] {
        assert!(KeyMap::parse(v).is_ok(), "{v}");
    }
    for v in ["A=B,B=A", "ZL=L,minus=none", "A=Shake"] {
        assert!(PadMap::parse(v).is_ok(), "{v}");
    }
}

#[test]
fn invalid_values() {
    let keymap = [
        "Q=1", "A=40,Q=1", "A=128", "A=abc", "A=-1", "A=", "A", "A=B", "", ",",
    ];
    for v in keymap {
        assert!(KeyMap::parse(v).is_err(), "keymap {v}");
    }
    let padmap = ["A=LUp", "Shake=B", "A=Q", "A=40", "A=40,B=38", ""];
    for v in padmap {
        assert!(PadMap::parse(v).is_err(), "pad map {v}");
    }
}

#[test]
fn a_key_code_has_at_most_seven_digits() {
    assert_eq!(
        KeyMap::parse("A=0000040").unwrap(),
        KeyMap::parse("A=40").unwrap()
    );
    assert!(KeyMap::parse("A=00000040").is_err());
    assert_eq!(
        keyed("A=0000040").get(Source::Key(usage::letter(b'k'))),
        Some(Action::Button(btn::A))
    );
}
