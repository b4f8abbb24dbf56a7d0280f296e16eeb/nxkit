//! Touch screen and gesture functions. The host pushes the touch points through [`crate::set_touches`].

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use super::{count, put_all};
use crate::with;

/// Writes a `TouchScreenState<N>`. The touch screen is out of reach when docked, so the state has only its header then.
fn read<const N: usize>(out: u64) {
    let bytes = with(|i| i.touch_state(N));
    let _ = guest::write(out, &bytes);
}

extern "C" fn get_state<const N: usize>(out: u64) {
    read::<N>(out);
}

/// The touch screen has one state at a time, so the call returns 1 entry.
extern "C" fn get_states<const N: usize>(out: u64, n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    read::<N>(out);
    1
}

extern "C" fn get_gesture_states(out: u64, n: i32) -> i32 {
    let list = with(|i| i.gestures(count(n)));
    put_all(out, &list);
    list.len() as i32
}

stubs! { stub_list;
    "_ZN2nn3hid21InitializeTouchScreenEv" => initialize_touch_screen() = ();
    "_ZN2nn3hid21InitializeTouchScreenEii" => initialize_touch_screen_size(u32, u32) = ();
    "_ZN2nn3hid17InitializeGestureEv" => initialize_gesture() = ();
}

macro_rules! sized {
    ($v:ident; $($n:literal)*) => {
        $(
            $v.push(Export::new(
                concat!("_ZN2nn3hid19GetTouchScreenStateILm", $n, "EEEvPNS0_16TouchScreenStateIXT_EEE"),
                get_state::<$n> as usize,
            ));
            $v.push(Export::new(
                concat!("_ZN2nn3hid20GetTouchScreenStatesILm", $n, "EEEiPNS0_16TouchScreenStateIXT_EEEi"),
                get_states::<$n> as usize,
            ));
        )*
    };
}

/// The touch screen and gesture functions. The touch screen state comes in sizes of 1 to 16 touches.
pub(super) fn list() -> Vec<Export> {
    let mut v = vec![Export::new(
        "_ZN2nn3hid16GetGestureStatesEPNS0_12GestureStateEi",
        get_gesture_states as usize,
    )];
    sized!(v; 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16);
    v.extend(stub_list());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;

    #[test]
    fn there_are_sixteen_sizes_with_two_functions_each() {
        let names: Vec<&str> = list().iter().map(|e| e.name).collect();
        assert_eq!(
            names
                .iter()
                .filter(|n| n.contains("GetTouchScreenState"))
                .count(),
            32
        );
        assert!(
            names.contains(
                &"_ZN2nn3hid19GetTouchScreenStateILm16EEEvPNS0_16TouchScreenStateIXT_EEE"
            )
        );
        assert!(
            names.contains(
                &"_ZN2nn3hid20GetTouchScreenStatesILm1EEEiPNS0_16TouchScreenStateIXT_EEEi"
            )
        );
    }

    #[test]
    fn docked_the_state_has_a_header_and_no_touches() {
        let (base, _guard) = guest_memory();
        guest::fill(base, 0x400, 0xff).ok();
        assert_eq!(get_states::<4>(base, 1), 1);
        assert_eq!(guest::read_u32(base + 8).unwrap_or(1), 0);
        assert_eq!(get_states::<4>(base, 0), 0);
    }

    #[test]
    fn docked_there_are_no_gestures() {
        let (base, _guard) = guest_memory();
        assert_eq!(get_gesture_states(base, 4), 0);
    }
}
