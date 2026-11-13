//! GameController through objc2. The controller, motion, keyboard and mouse reads of the crate are here, and so is
//! the `unsafe` they need. Rumble is in `haptics`.
//!
//! Every read is made at the moment of the call. Nothing polls and no handler is installed (R39).

use objc2::rc::Retained;
use objc2_game_controller::*;

use super::haptics::Haptics;
use crate::config::Config;
use crate::keys::KeySet;
use crate::npad::{PLAYERS, RawPad, btn};
use crate::rumble::PlayerLevels;
use crate::slots::Slots;

/// A connected controller that has an extended gamepad profile.
struct Pad {
    controller: Retained<GCController>,
    gamepad: Retained<GCExtendedGamepad>,
    motion: Option<Retained<GCMotion>>,
    /// True when the face buttons carry the Nintendo names, so A is A. Other pads have the Xbox layout.
    labelled: bool,
    haptics: Haptics,
}

/// The controllers, the keyboard and the mouse of the Mac.
pub(crate) struct Platform {
    pads: [Option<Pad>; PLAYERS],
    motion: bool,
    rumble: bool,
    keyboard: bool,
    keys: Vec<(u8, GCKeyCode)>,
}

// SAFETY: the Objective-C objects in `Platform` are only reached through the one `Mutex<Input>` of the crate, so no
// two threads use them at the same time. GameController documents its controller objects as usable from any thread
// when the use is not concurrent.
unsafe impl Send for Platform {}

fn down(b: &GCControllerButtonInput) -> bool {
    // SAFETY: `isPressed` takes no arguments.
    unsafe { b.isPressed() }
}

fn down_opt(b: Option<Retained<GCControllerButtonInput>>) -> bool {
    b.is_some_and(|b| down(&b))
}

fn value(a: &GCControllerAxisInput) -> f32 {
    // SAFETY: `value` takes no arguments.
    unsafe { a.value() }
}

macro_rules! key_table {
    ($($usage:expr => $code:ident),* $(,)?) => {
        /// The USB usage id of every key that the service reads, with its GameController key code.
        fn key_table() -> Vec<(u8, GCKeyCode)> {
            // SAFETY: the key codes are constants that GameController defines and that never change.
            unsafe { vec![$(($usage, $code)),*] }
        }
    };
}

key_table! {
    0x04 => GCKeyCodeKeyA, 0x05 => GCKeyCodeKeyB, 0x06 => GCKeyCodeKeyC, 0x07 => GCKeyCodeKeyD,
    0x08 => GCKeyCodeKeyE, 0x09 => GCKeyCodeKeyF, 0x0a => GCKeyCodeKeyG, 0x0b => GCKeyCodeKeyH,
    0x0c => GCKeyCodeKeyI, 0x0d => GCKeyCodeKeyJ, 0x0e => GCKeyCodeKeyK, 0x0f => GCKeyCodeKeyL,
    0x10 => GCKeyCodeKeyM, 0x11 => GCKeyCodeKeyN, 0x12 => GCKeyCodeKeyO, 0x13 => GCKeyCodeKeyP,
    0x14 => GCKeyCodeKeyQ, 0x15 => GCKeyCodeKeyR, 0x16 => GCKeyCodeKeyS, 0x17 => GCKeyCodeKeyT,
    0x18 => GCKeyCodeKeyU, 0x19 => GCKeyCodeKeyV, 0x1a => GCKeyCodeKeyW, 0x1b => GCKeyCodeKeyX,
    0x1c => GCKeyCodeKeyY, 0x1d => GCKeyCodeKeyZ,
    0x1e => GCKeyCodeOne, 0x1f => GCKeyCodeTwo, 0x20 => GCKeyCodeThree, 0x21 => GCKeyCodeFour,
    0x22 => GCKeyCodeFive, 0x23 => GCKeyCodeSix, 0x24 => GCKeyCodeSeven, 0x25 => GCKeyCodeEight,
    0x26 => GCKeyCodeNine, 0x27 => GCKeyCodeZero,
    0x28 => GCKeyCodeReturnOrEnter, 0x29 => GCKeyCodeEscape, 0x2a => GCKeyCodeDeleteOrBackspace,
    0x2b => GCKeyCodeTab, 0x2c => GCKeyCodeSpacebar, 0x2d => GCKeyCodeHyphen, 0x2e => GCKeyCodeEqualSign,
    0x2f => GCKeyCodeOpenBracket, 0x30 => GCKeyCodeCloseBracket, 0x31 => GCKeyCodeBackslash,
    0x32 => GCKeyCodeNonUSPound, 0x33 => GCKeyCodeSemicolon, 0x34 => GCKeyCodeQuote,
    0x35 => GCKeyCodeGraveAccentAndTilde, 0x36 => GCKeyCodeComma, 0x37 => GCKeyCodePeriod,
    0x38 => GCKeyCodeSlash, 0x39 => GCKeyCodeCapsLock,
    0x3a => GCKeyCodeF1, 0x3b => GCKeyCodeF2, 0x3c => GCKeyCodeF3, 0x3d => GCKeyCodeF4,
    0x3e => GCKeyCodeF5, 0x3f => GCKeyCodeF6, 0x40 => GCKeyCodeF7, 0x41 => GCKeyCodeF8,
    0x42 => GCKeyCodeF9, 0x43 => GCKeyCodeF10, 0x44 => GCKeyCodeF11, 0x45 => GCKeyCodeF12,
    0x46 => GCKeyCodePrintScreen, 0x47 => GCKeyCodeScrollLock, 0x48 => GCKeyCodePause,
    0x49 => GCKeyCodeInsert, 0x4a => GCKeyCodeHome, 0x4b => GCKeyCodePageUp,
    0x4c => GCKeyCodeDeleteForward, 0x4d => GCKeyCodeEnd, 0x4e => GCKeyCodePageDown,
    0x4f => GCKeyCodeRightArrow, 0x50 => GCKeyCodeLeftArrow, 0x51 => GCKeyCodeDownArrow,
    0x52 => GCKeyCodeUpArrow,
    0xe0 => GCKeyCodeLeftControl, 0xe1 => GCKeyCodeLeftShift, 0xe2 => GCKeyCodeLeftAlt,
    0xe3 => GCKeyCodeLeftGUI, 0xe4 => GCKeyCodeRightControl, 0xe5 => GCKeyCodeRightShift,
    0xe6 => GCKeyCodeRightAlt, 0xe7 => GCKeyCodeRightGUI,
}

/// True for the Nintendo pads, whose face buttons are named as on the Switch.
fn is_labelled(category: &str) -> bool {
    category == "Switch Pro Controller" || category.contains("Joy-Con")
}

impl Platform {
    pub(crate) fn new(config: &Config) -> Platform {
        Platform {
            pads: std::array::from_fn(|_| None),
            motion: config.motion,
            rumble: config.rumble,
            keyboard: config.keyboard,
            keys: if config.keyboard {
                key_table()
            } else {
                Vec::new()
            },
        }
    }

    /// Takes the controllers that came and drops the ones that left. A controller keeps its slot while it stays.
    pub(crate) fn refresh(&mut self, slots: &mut Slots) {
        // SAFETY: `controllers` is a class method without arguments.
        let list = unsafe { GCController::controllers() }.to_vec();
        let live: Vec<usize> = list.iter().map(|c| Retained::as_ptr(c) as usize).collect();
        for p in 0..PLAYERS {
            if let Some(owner) = slots.owner_of(p)
                && !live.contains(&owner)
            {
                slots.free(owner);
                self.pads[p] = None;
            }
        }
        for controller in list {
            let owner = Retained::as_ptr(&controller) as usize;
            if slots.slot_of(owner).is_some() {
                continue;
            }
            // SAFETY: property reads on a live controller.
            let (gamepad, name, category) = unsafe {
                (
                    controller.extendedGamepad(),
                    controller
                        .vendorName()
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    controller.productCategory().to_string(),
                )
            };
            let Some(gamepad) = gamepad else { continue };
            let Some(slot) = slots.take(owner, &name) else {
                continue;
            };
            self.pads[slot] = Some(Pad {
                motion: self.attach_motion(&controller),
                labelled: is_labelled(&category),
                haptics: Haptics::default(),
                controller,
                gamepad,
            });
        }
    }

    /// The motion sensors of a controller that has a gyro. Sensors that need activation are switched on.
    fn attach_motion(&self, controller: &GCController) -> Option<Retained<GCMotion>> {
        if !self.motion {
            return None;
        }
        // SAFETY: property reads and one setter on a live `GCMotion`.
        unsafe {
            let motion = controller.motion()?;
            if !motion.hasRotationRate() {
                return None;
            }
            if motion.sensorsRequireManualActivation() {
                motion.setSensorsActive(true);
            }
            Some(motion)
        }
    }

    /// The buttons and sticks of the controller in `slot`, mapped to the Switch.
    pub(crate) fn read_pad(&self, slot: usize) -> Option<RawPad> {
        let physical = self.read_physical_pad(slot);
        #[cfg(target_os = "ios")]
        let physical =
            super::virtual_pad::merge_player(slot, physical, super::virtual_pad::snapshot());
        physical
    }

    fn read_physical_pad(&self, slot: usize) -> Option<RawPad> {
        let pad = self.pads.get(slot)?.as_ref()?;
        let g = &pad.gamepad;
        // SAFETY: property reads on a live `GCExtendedGamepad` and the elements it returns.
        unsafe {
            let (a, b, x, y) = if pad.labelled {
                (g.buttonA(), g.buttonB(), g.buttonX(), g.buttonY())
            } else {
                (g.buttonB(), g.buttonA(), g.buttonY(), g.buttonX())
            };
            let dpad = g.dpad();
            let (ls, rs) = (g.leftThumbstick(), g.rightThumbstick());
            let pairs = [
                (down(&a), btn::A),
                (down(&b), btn::B),
                (down(&x), btn::X),
                (down(&y), btn::Y),
                (down_opt(g.leftThumbstickButton()), btn::STICK_L),
                (down_opt(g.rightThumbstickButton()), btn::STICK_R),
                (down(&g.leftShoulder()), btn::L),
                (down(&g.rightShoulder()), btn::R),
                (down(&g.leftTrigger()), btn::ZL),
                (down(&g.rightTrigger()), btn::ZR),
                (down(&g.buttonMenu()), btn::PLUS),
                (down_opt(g.buttonOptions()), btn::MINUS),
                (down(&dpad.left()), btn::LEFT),
                (down(&dpad.up()), btn::UP),
                (down(&dpad.right()), btn::RIGHT),
                (down(&dpad.down()), btn::DOWN),
            ];
            let buttons = pairs
                .iter()
                .filter(|(on, _)| *on)
                .fold(0, |m, (_, b)| m | b);
            let axes = [
                value(&ls.xAxis()),
                value(&ls.yAxis()),
                value(&rs.xAxis()),
                value(&rs.yAxis()),
            ];
            Some(RawPad { buttons, axes })
        }
    }

    /// Acceleration in G and rotation rate in rad/s of the controller in `slot`. `None` for a controller without a
    /// gyro or one that has not sent a sample yet.
    pub(crate) fn read_motion(&self, slot: usize) -> Option<([f32; 3], [f32; 3])> {
        let m = self.pads.get(slot)?.as_ref()?.motion.as_ref()?;
        // SAFETY: property reads on a live `GCMotion`.
        let (acc, rot) = unsafe {
            let acc = if m.hasGravityAndUserAcceleration() {
                let (g, u) = (m.gravity(), m.userAcceleration());
                [g.x + u.x, g.y + u.y, g.z + u.z]
            } else {
                let a = m.acceleration();
                [a.x, a.y, a.z]
            };
            let r = m.rotationRate();
            (acc, [r.x, r.y, r.z])
        };
        if acc.iter().chain(rot.iter()).all(|v| *v == 0.0) {
            return None;
        }
        Some((acc.map(|v| v as f32), rot.map(|v| v as f32)))
    }

    /// The keys that are down. `None` when the keyboard is off or none is connected.
    pub(crate) fn read_keys(&self) -> Option<KeySet> {
        if !self.keyboard {
            return None;
        }
        // SAFETY: class method and property reads.
        let input = unsafe { GCKeyboard::coalescedKeyboard()?.keyboardInput()? };
        let mut set = KeySet::default();
        for &(key, code) in &self.keys {
            // SAFETY: `buttonForKeyCode` takes a key code that GameController defines.
            if down_opt(unsafe { input.buttonForKeyCode(code) }) {
                set.set(key);
            }
        }
        Some(set)
    }

    /// The `MouseButton` bits of the mice. `None` when the mouse is off or none is connected.
    pub(crate) fn read_mouse(&self) -> Option<u32> {
        if !self.keyboard {
            return None;
        }
        // SAFETY: class method without arguments.
        let mice = unsafe { GCMouse::mice() }.to_vec();
        if mice.is_empty() {
            return None;
        }
        let mut buttons = 0;
        for mouse in mice {
            // SAFETY: property reads on a live mouse and its buttons.
            unsafe {
                if let Some(input) = mouse.mouseInput() {
                    buttons |= u32::from(down(&input.leftButton()));
                    buttons |= u32::from(down_opt(input.rightButton())) << 1;
                    buttons |= u32::from(down_opt(input.middleButton())) << 2;
                }
            }
        }
        Some(buttons)
    }

    /// Plays the levels of a player on its controller.
    pub(crate) fn rumble(&mut self, slot: usize, levels: &PlayerLevels) {
        if !self.rumble {
            return;
        }
        if let Some(Some(pad)) = self.pads.get_mut(slot) {
            pad.haptics.update(&pad.controller, levels);
        }
    }
}
