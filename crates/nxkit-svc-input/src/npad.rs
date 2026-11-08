//! Npad ids, styles, the state layout and how a controller reading becomes an `NpadState`.
//!
//! The rules are those of nxnative `hle_hid.c` (MIT). Only the full key and the handheld style ever report a
//! controller. The other styles answer with zeros and no attributes.

/// The id of the handheld npad.
pub(crate) const ID_HANDHELD: u32 = 0x20;
/// The id of the `Other` npad.
pub(crate) const ID_OTHER: u32 = 0x10;
/// The number of players, `No1` to `No8`.
pub(crate) const PLAYERS: usize = 8;

/// The style bits of `nn::hid::NpadStyleSet`.
pub(crate) mod style {
    /// `NpadStyleFullKey`
    pub(crate) const FULL_KEY: u32 = 1;
    /// `NpadStyleHandheld`
    pub(crate) const HANDHELD: u32 = 1 << 1;
    /// `NpadStyleJoyDual`
    pub(crate) const JOY_DUAL: u32 = 1 << 2;
    /// `NpadStyleJoyLeft`
    pub(crate) const JOY_LEFT: u32 = 1 << 3;
    /// `NpadStyleJoyRight`
    pub(crate) const JOY_RIGHT: u32 = 1 << 4;
}

/// The button bits of `nn::hid::NpadButton`.
pub(crate) mod btn {
    pub(crate) const A: u64 = 1 << 0;
    pub(crate) const B: u64 = 1 << 1;
    pub(crate) const X: u64 = 1 << 2;
    pub(crate) const Y: u64 = 1 << 3;
    pub(crate) const STICK_L: u64 = 1 << 4;
    pub(crate) const STICK_R: u64 = 1 << 5;
    pub(crate) const L: u64 = 1 << 6;
    pub(crate) const R: u64 = 1 << 7;
    pub(crate) const ZL: u64 = 1 << 8;
    pub(crate) const ZR: u64 = 1 << 9;
    pub(crate) const PLUS: u64 = 1 << 10;
    pub(crate) const MINUS: u64 = 1 << 11;
    pub(crate) const LEFT: u64 = 1 << 12;
    pub(crate) const UP: u64 = 1 << 13;
    pub(crate) const RIGHT: u64 = 1 << 14;
    pub(crate) const DOWN: u64 = 1 << 15;
    pub(crate) const STICK_L_LEFT: u64 = 1 << 16;
    pub(crate) const STICK_L_UP: u64 = 1 << 17;
    pub(crate) const STICK_L_RIGHT: u64 = 1 << 18;
    pub(crate) const STICK_L_DOWN: u64 = 1 << 19;
    pub(crate) const STICK_R_LEFT: u64 = 1 << 20;
    pub(crate) const STICK_R_UP: u64 = 1 << 21;
    pub(crate) const STICK_R_RIGHT: u64 = 1 << 22;
    pub(crate) const STICK_R_DOWN: u64 = 1 << 23;
}

/// `NpadAttribute` bits: connected, wired, and the same for the left and the right joy-con.
pub(crate) const ATTR_CONNECTED: u32 = 1;
const ATTR_LEFT_CONNECTED: u32 = 1 << 2;
const ATTR_RIGHT_CONNECTED: u32 = 1 << 4;
const ATTR_HANDHELD: u32 = 0x3f;

/// The size of one `NpadXxxState` in bytes.
pub(crate) const STATE_SIZE: usize = 0x28;

/// The stick value from which a stick also presses its direction button.
const STICK_BUTTON_THRESHOLD: i32 = 0x4000;

/// The bit of an npad id in the supported-id mask. Ids 0 to 7 are the bits 0 to 7, handheld is 8 and `Other` is 9.
pub(crate) fn id_bit(id: u32) -> u32 {
    match id {
        0..=7 => 1 << id,
        ID_HANDHELD => 1 << 8,
        ID_OTHER => 1 << 9,
        _ => 0,
    }
}

/// The state of one npad as the game reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct NpadState {
    /// `NpadButton` bits.
    pub(crate) buttons: u64,
    /// The left stick, -32767 to 32767.
    pub(crate) lx: i32,
    pub(crate) ly: i32,
    /// The right stick.
    pub(crate) rx: i32,
    pub(crate) ry: i32,
    /// `NpadAttribute` bits.
    pub(crate) attributes: u32,
}

impl NpadState {
    /// The 0x28 bytes of the state with the sampling number `sampling`.
    pub(crate) fn to_bytes(self, sampling: i64) -> [u8; STATE_SIZE] {
        let mut b = [0u8; STATE_SIZE];
        b[0..8].copy_from_slice(&sampling.to_le_bytes());
        b[8..16].copy_from_slice(&self.buttons.to_le_bytes());
        b[16..20].copy_from_slice(&self.lx.to_le_bytes());
        b[20..24].copy_from_slice(&self.ly.to_le_bytes());
        b[24..28].copy_from_slice(&self.rx.to_le_bytes());
        b[28..32].copy_from_slice(&self.ry.to_le_bytes());
        b[32..36].copy_from_slice(&self.attributes.to_le_bytes());
        b
    }
}

/// A controller reading before it is mapped to the npad ids: the buttons in the `NpadButton` bits (0 to 15) and the
/// two sticks as -1 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RawPad {
    /// `NpadButton` bits 0 to 15.
    pub(crate) buttons: u64,
    /// Left x, left y, right x, right y.
    pub(crate) axes: [f32; 4],
}

/// A stick value in -1 to 1 as -32767 to 32767.
pub(crate) fn to_stick(v: f32) -> i32 {
    if !v.is_finite() {
        return 0;
    }
    (v * 32767.0).clamp(-32767.0, 32767.0) as i32
}

/// The value with the larger magnitude. The first wins a tie.
pub(crate) fn pick(a: i32, b: i32) -> i32 {
    if b.abs() > a.abs() { b } else { a }
}

/// The direction buttons that two sticks press.
pub(crate) fn stick_buttons(lx: i32, ly: i32, rx: i32, ry: i32) -> u64 {
    let t = STICK_BUTTON_THRESHOLD;
    let mut b = 0;
    if lx < -t {
        b |= btn::STICK_L_LEFT;
    }
    if ly > t {
        b |= btn::STICK_L_UP;
    }
    if lx > t {
        b |= btn::STICK_L_RIGHT;
    }
    if ly < -t {
        b |= btn::STICK_L_DOWN;
    }
    if rx < -t {
        b |= btn::STICK_R_LEFT;
    }
    if ry > t {
        b |= btn::STICK_R_UP;
    }
    if rx > t {
        b |= btn::STICK_R_RIGHT;
    }
    if ry < -t {
        b |= btn::STICK_R_DOWN;
    }
    b
}

/// The attributes of a connected pad of `style`.
pub(crate) fn attributes_for(style: u32) -> u32 {
    if style == style::HANDHELD {
        ATTR_HANDHELD
    } else {
        ATTR_CONNECTED | ATTR_LEFT_CONNECTED | ATTR_RIGHT_CONNECTED
    }
}

/// Which npad ids exist, as the game set them up and as controllers are connected.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    /// True when the console is in handheld mode: player 1 is the handheld npad.
    pub(crate) handheld: bool,
    /// The mask of `SetSupportedNpadIdType`, in [`id_bit`] bits.
    pub(crate) ids: u32,
    /// Bit `p` is set when player `p` has a controller.
    pub(crate) connected: u8,
}

impl Layout {
    /// The npad id of player 1.
    pub(crate) fn connected_id(&self) -> u32 {
        if self.handheld { ID_HANDHELD } else { 0 }
    }

    /// True when player `p` shows up as connected. Player 1 always does.
    pub(crate) fn reported(&self, p: usize) -> bool {
        p == 0 || (p < PLAYERS && self.connected >> p & 1 == 1 && self.ids & id_bit(p as u32) != 0)
    }

    /// The player behind an npad id, if the id is in use.
    pub(crate) fn player_of(&self, id: u32) -> Option<usize> {
        if id == self.connected_id() {
            Some(0)
        } else if (1..PLAYERS as u32).contains(&id) && self.reported(id as usize) {
            Some(id as usize)
        } else {
            None
        }
    }

    /// The style that reports a controller on `id`. 0 when no player is behind it.
    pub(crate) fn style_for(id: u32, player: Option<usize>) -> u32 {
        match player {
            None => 0,
            Some(_) if id == ID_HANDHELD => style::HANDHELD,
            Some(_) => style::FULL_KEY,
        }
    }

    /// `GetNpadStyleSet` of an id.
    pub(crate) fn style_of(&self, id: u32) -> u32 {
        Self::style_for(id, self.player_of(id))
    }

    /// True when the game supports no player but the first. Controllers of other slots then feed player 1.
    pub(crate) fn one_player(&self) -> bool {
        self.ids & 0xfe == 0
    }

    /// The number of players that show up as connected.
    pub(crate) fn reported_count(&self) -> u32 {
        (0..PLAYERS).filter(|&p| self.reported(p)).count() as u32
    }
}

/// Builds the state of an npad from a reading. `pad` is the controller, `keys` the keyboard for player 1.
pub(crate) fn build_state(style: u32, pad: &RawPad, keys: Option<&RawKeys>) -> NpadState {
    let mut buttons = pad.buttons;
    let mut lx = to_stick(pad.axes[0]);
    let mut ly = to_stick(pad.axes[1]);
    let mut rx = to_stick(pad.axes[2]);
    let mut ry = to_stick(pad.axes[3]);
    if let Some(k) = keys {
        buttons |= k.buttons;
        lx = pick(lx, k.sticks[0]);
        ly = pick(ly, k.sticks[1]);
        rx = pick(rx, k.sticks[2]);
        ry = pick(ry, k.sticks[3]);
    }
    buttons |= stick_buttons(lx, ly, rx, ry);
    NpadState {
        buttons,
        lx,
        ly,
        rx,
        ry,
        attributes: attributes_for(style),
    }
}

/// What the keyboard gives player 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RawKeys {
    /// `NpadButton` bits 0 to 15.
    pub(crate) buttons: u64,
    /// Left x, left y, right x, right y, as -32767 to 32767.
    pub(crate) sticks: [i32; 4],
}

/// Merges the readings of several controllers into one, for a game with one player. Buttons are ORed and a stick
/// takes the larger deflection.
pub(crate) fn merge(pads: &[RawPad]) -> RawPad {
    let mut out = RawPad::default();
    for p in pads {
        out.buttons |= p.buttons;
        for i in 0..4 {
            if p.axes[i].abs() > out.axes[i].abs() {
                out.axes[i] = p.axes[i];
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(handheld: bool, ids: u32, connected: u8) -> Layout {
        Layout {
            handheld,
            ids,
            connected,
        }
    }

    #[test]
    fn id_bits() {
        assert_eq!(id_bit(0), 1);
        assert_eq!(id_bit(7), 0x80);
        assert_eq!(id_bit(ID_HANDHELD), 0x100);
        assert_eq!(id_bit(ID_OTHER), 0x200);
        assert_eq!(id_bit(9), 0);
    }

    #[test]
    fn handheld_mode_moves_player_one_to_the_handheld_id() {
        let l = layout(true, 0x3ff, 1);
        assert_eq!(l.player_of(ID_HANDHELD), Some(0));
        assert_eq!(l.player_of(0), None);
        assert_eq!(l.style_of(ID_HANDHELD), style::HANDHELD);
        assert_eq!(l.style_of(0), 0);
        let l = layout(false, 0x3ff, 1);
        assert_eq!(l.style_of(0), style::FULL_KEY);
    }

    #[test]
    fn a_second_player_needs_a_controller_and_a_supported_id() {
        assert_eq!(layout(false, 0x3ff, 1).player_of(1), None);
        assert_eq!(layout(false, 0x3ff, 0b11).player_of(1), Some(1));
        assert_eq!(layout(false, 0x001, 0b11).player_of(1), None);
    }

    #[test]
    fn one_player_games_support_only_the_first_id() {
        assert!(layout(false, 0x101, 1).one_player());
        assert!(!layout(false, 0x003, 1).one_player());
    }

    #[test]
    fn sticks_press_direction_buttons_past_the_threshold() {
        assert_eq!(stick_buttons(0, 0, 0, 0), 0);
        assert_eq!(
            stick_buttons(-0x4001, 0x4001, 0, 0),
            btn::STICK_L_LEFT | btn::STICK_L_UP
        );
        assert_eq!(stick_buttons(0x4000, 0, 0, 0), 0);
        assert_eq!(
            stick_buttons(0, 0, 0x4001, -0x4001),
            btn::STICK_R_RIGHT | btn::STICK_R_DOWN
        );
    }

    #[test]
    fn the_state_has_the_horizon_layout() {
        let s = NpadState {
            buttons: 0x0102,
            lx: -5,
            ly: 6,
            rx: 7,
            ry: -8,
            attributes: 0x15,
        };
        let b = s.to_bytes(0x1_0001);
        assert_eq!(&b[0..8], &0x1_0001i64.to_le_bytes());
        assert_eq!(&b[8..16], &0x0102u64.to_le_bytes());
        assert_eq!(&b[16..20], &(-5i32).to_le_bytes());
        assert_eq!(&b[32..36], &0x15u32.to_le_bytes());
        assert_eq!(&b[36..40], &[0; 4]);
    }

    #[test]
    fn the_keyboard_adds_to_the_controller() {
        let pad = RawPad {
            buttons: btn::A,
            axes: [0.5, 0.0, 0.0, 0.0],
        };
        let keys = RawKeys {
            buttons: btn::B,
            sticks: [-32767, 0, 0, 0],
        };
        let s = build_state(style::FULL_KEY, &pad, Some(&keys));
        assert_eq!(s.buttons & (btn::A | btn::B), btn::A | btn::B);
        assert_eq!(s.lx, -32767);
        assert_ne!(s.buttons & btn::STICK_L_LEFT, 0);
        assert_eq!(s.attributes, 0x15);
    }

    #[test]
    fn handheld_sets_every_attribute() {
        assert_eq!(attributes_for(style::HANDHELD), 0x3f);
    }

    #[test]
    fn merging_takes_the_larger_deflection() {
        let a = RawPad {
            buttons: btn::A,
            axes: [0.2, -0.9, 0.0, 0.0],
        };
        let b = RawPad {
            buttons: btn::B,
            axes: [-0.5, 0.3, 0.0, 0.0],
        };
        let m = merge(&[a, b]);
        assert_eq!(m.buttons, btn::A | btn::B);
        assert_eq!(m.axes, [-0.5, -0.9, 0.0, 0.0]);
    }
}
