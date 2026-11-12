//! Whole-snapshot bridge from UIKit to nxkit's existing Npad path.
use crate::npad::{RawPad, merge};
#[cfg(target_os = "ios")]
use std::sync::{Mutex, PoisonError};

// Only actual Npad buttons. Stick direction bits are derived by nxkit itself.
const BUTTON_MASK: u64 = 0xffff;

#[derive(Default)]
struct VirtualPad {
    active: bool,
    pad: RawPad,
}
impl VirtualPad {
    fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.pad = RawPad::default();
        }
    }
    fn update(&mut self, buttons: u64, axes: [f32; 4]) {
        self.pad = if self.active {
            RawPad {
                buttons: buttons & BUTTON_MASK,
                axes: axes.map(|v| {
                    if v.is_finite() {
                        v.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    }
                }),
            }
        } else {
            RawPad::default()
        };
    }
}
#[cfg(target_os = "ios")]
static VIRTUAL: Mutex<VirtualPad> = Mutex::new(VirtualPad {
    active: false,
    pad: RawPad {
        buttons: 0,
        axes: [0.0; 4],
    },
});

/// Replaces the complete virtual P1 state. Axes are x-right/y-up in [-1, 1].
/// Focus loss clears the snapshot and ignores later input until reactivation.
#[cfg(target_os = "ios")]
pub fn set_virtual_pad(buttons: u64, lx: f32, ly: f32, rx: f32, ry: f32) {
    VIRTUAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .update(buttons, [lx, ly, rx, ry]);
}
#[cfg(target_os = "ios")]
pub(crate) fn set_active(active: bool) {
    VIRTUAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set_active(active);
}
#[cfg(target_os = "ios")]
pub(crate) fn snapshot() -> RawPad {
    VIRTUAL.lock().unwrap_or_else(PoisonError::into_inner).pad
}
pub(crate) fn merge_player(
    slot: usize,
    physical: Option<RawPad>,
    virtual_pad: RawPad,
) -> Option<RawPad> {
    if slot != 0 {
        return physical;
    }
    // The upstream merge ORs buttons and takes the strongest magnitude per
    // axis. Put physical first so hardware wins equal-magnitude conflicts.
    Some(merge(&[physical.unwrap_or_default(), virtual_pad]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::npad::{btn, build_state, style};

    #[test]
    fn sanitizes_buttons_and_all_four_axes() {
        let mut v = VirtualPad::default();
        v.set_active(true);
        v.update(u64::MAX, [f32::NAN, f32::INFINITY, -9.0, 9.0]);
        assert_eq!(v.pad.buttons, 0xffff);
        assert_eq!(v.pad.axes, [0.0, 0.0, -1.0, 1.0]);
        v.update(0, [f32::NEG_INFINITY, 0.25, -0.5, 0.0]);
        assert_eq!(v.pad.axes, [0.0, 0.25, -0.5, 0.0]);
    }
    #[test]
    fn loss_of_focus_releases_and_blocks_stale_callbacks() {
        let mut v = VirtualPad::default();
        v.update(btn::A, [1.0; 4]);
        assert_eq!(v.pad.buttons, 0);
        v.set_active(true);
        v.update(btn::A, [1.0; 4]);
        v.set_active(false);
        v.update(btn::B, [-1.0; 4]);
        v.set_active(true);
        assert_eq!(v.pad.buttons, 0);
        assert_eq!(v.pad.axes, [0.0; 4]);
    }
    #[test]
    fn physical_connect_disconnect_preserves_virtual_p1_and_other_players() {
        let virtual_pad = RawPad {
            buttons: btn::A,
            axes: [0.5, 1.0, 0.0, 0.0],
        };
        let physical = RawPad {
            buttons: btn::B,
            axes: [-0.5, 0.25, -1.0, 0.0],
        };
        let p1 = merge_player(0, Some(physical), virtual_pad).unwrap();
        assert_eq!(p1.buttons, btn::A | btn::B);
        assert_eq!(p1.axes, [-0.5, 1.0, -1.0, 0.0]);
        let disconnected = merge_player(0, None, virtual_pad).unwrap();
        assert_eq!(disconnected.buttons, btn::A);
        assert_eq!(disconnected.axes, virtual_pad.axes);
        let other = merge_player(1, Some(physical), virtual_pad).unwrap();
        assert_eq!(other.buttons, btn::B);
        assert_eq!(other.axes, physical.axes);
        assert!(merge_player(1, None, virtual_pad).is_none());
        // Releasing the overlay never releases hardware buttons or sticks.
        let released = merge_player(0, Some(physical), RawPad::default()).unwrap();
        assert_eq!(released.buttons, btn::B);
        assert_eq!(released.axes, physical.axes);
    }
    #[test]
    fn feeds_real_npad_buttons_sticks_and_derived_directions() {
        let mut v = VirtualPad::default();
        v.set_active(true);
        v.update(btn::A | btn::ZR | btn::PLUS, [-1.0, 1.0, 1.0, -1.0]);
        let raw = merge_player(0, None, v.pad).unwrap();
        for style in [style::HANDHELD, style::FULL_KEY] {
            let state = build_state(style, &raw, None);
            assert_eq!(
                (state.lx, state.ly, state.rx, state.ry),
                (-32767, 32767, 32767, -32767)
            );
            assert_eq!(
                state.buttons,
                btn::A
                    | btn::ZR
                    | btn::PLUS
                    | btn::STICK_L_LEFT
                    | btn::STICK_L_UP
                    | btn::STICK_R_RIGHT
                    | btn::STICK_R_DOWN
            );
            assert_ne!(state.attributes, 0);
        }
        v.update(0, [0.0; 4]);
        assert_eq!(build_state(style::HANDHELD, &v.pad, None).buttons, 0);
    }
}
