//! The Nmouse and Joy-Con mouse sensor (Jcms) functions of the Switch 2 SDKs.
//!
//! The host has no pointer device and no Joy-Con 2, so there are no handles and no states. The functions take what
//! the game sets and report nothing. The pointer range is the one function pair with a value to keep.

use std::sync::atomic::{AtomicI32, Ordering};

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use super::read_ref;

static RANGE_W: AtomicI32 = AtomicI32::new(0);
static RANGE_H: AtomicI32 = AtomicI32::new(0);

fn set_range(w: i32, h: i32) {
    RANGE_W.store(w, Ordering::Relaxed);
    RANGE_H.store(h, Ordering::Relaxed);
}

extern "C" fn initialize_with_type(_work: u64, _size: u64, w: u32, h: u32, _kind: u32) {
    set_range(w as i32, h as i32);
}

extern "C" fn set_pointer_ranges(w: u32, h: u32) {
    set_range(w as i32, h as i32);
}

extern "C" fn get_pointer_ranges(w: u64, h: u64) {
    let _ = guest::write_u32(w, RANGE_W.load(Ordering::Relaxed) as u32);
    let _ = guest::write_u32(h, RANGE_H.load(Ordering::Relaxed) as u32);
}

/// The merged handle exists without a device and is 0.
extern "C" fn get_merged_handle(out: u64) -> i32 {
    let _ = guest::write_u64(out, 0);
    1
}

extern "C" fn get_type_set(out: u64, _handle: u64) -> i32 {
    let _ = guest::write_u32(out, 0);
    1
}

extern "C" fn get_states(count: u64, _out: u64, _n: u32, _handle: u64) -> i32 {
    let _ = guest::write_u32(count, 0);
    1
}

/// The merged handle takes the setting and reports nothing.
extern "C" fn takes_setting(handle: u64) -> u32 {
    u32::from(handle == 0)
}

extern "C" fn enable_pointer(handle: u64, _on: u32) -> u32 {
    takes_setting(handle)
}

extern "C" fn set_pointer_position(handle: u64, _x: u32, _y: u32) -> u32 {
    takes_setting(handle)
}

/// The handle of a Joy-Con mouse sensor: the npad id in byte 0 and the `JcmsType` in byte 1.
extern "C" fn get_jcms_handle(id: u64, kind: u64) -> u32 {
    (read_ref(id) & 0xff) | (read_ref(kind) & 0xff) << 8
}

stubs! { stub_list;
    "_ZN2nn3hid14FinalizeNmouseEv" => finalize() = ();
    "_ZN2nn3hid26SetMergedNmouseSourceTypesENS_4util10BitFlagSetILi32ENS0_10NmouseTypeEEE" => set_source_types(u32) = ();
    "_ZN2nn3hid16InitializeNmouseEPvmii" => initialize(u64, u64, u32, u32) = ();
    "_ZN2nn3hid16GetNmouseHandlesEPNS0_12NmouseHandleEij" => get_handles(u64, u32, u32) -> i32 = 0;
    "_ZN2nn3hid32SetNmouseButtonConfigJoyLeftJcmsERKNS0_29NmouseButtonConfigJoyLeftJcmsE" => button_config_left(u64) = ();
    "_ZN2nn3hid33SetNmouseButtonConfigJoyRightJcmsERKNS0_30NmouseButtonConfigJoyRightJcmsE" => button_config_right(u64) = ();
    "_ZN2nn3hid34SetNmouseButtonConfigStandardMouseERKNS0_31NmouseButtonConfigStandardMouseE" => button_config_mouse(u64) = ();
    "_ZN2nn3hid28SetNmousePointerMovementAreaEiiii" => set_movement_area(u32, u32, u32, u32) = ();
    "_ZN2nn3hid21RestrictNmousePointerEffff" => restrict_pointer() = ();
    "_ZN2nn3hid28GetStandardMouseNmouseHandleEPNS0_12NmouseHandleE" => get_standard_mouse_handle(u64) -> u32 = 0;
    "_ZN2nn3hid14GetNmouseStateEPNS0_11NmouseStateENS0_12NmouseHandleE" => get_state(u64, u64) -> u32 = 0;
    "_ZN2nn3hid13GetNpadIdTypeEPjNS0_12NmouseHandleE" => get_npad_id_type(u64, u64) -> u32 = 0;
    "_ZN2nn3hid14InitializeJcmsEPvm" => jcms_initialize(u64, u64) = ();
    "_ZN2nn3hid12FinalizeJcmsEv" => jcms_finalize() = ();
    "_ZN2nn3hid9StartJcmsERKNS0_10JcmsHandleE" => jcms_start(u64) = ();
    "_ZN2nn3hid8StopJcmsERKNS0_10JcmsHandleE" => jcms_stop(u64) = ();
    "_ZN2nn3hid13GetJcmsStatesEPNS0_9JcmsStateEiRKNS0_10JcmsHandleE" => jcms_states(u64, u32, u64) -> i32 = 0;
}

/// The Nmouse and Jcms functions.
pub(super) fn list() -> Vec<Export> {
    let mut v = vec![
        Export::new(
            "_ZN2nn3hid16InitializeNmouseEPvmiiNS0_17NmousePointerTypeE",
            initialize_with_type as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3hid22SetNmousePointerRangesEii",
            set_pointer_ranges as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3hid22GetNmousePointerRangesEPiS1_",
            get_pointer_ranges as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3hid21GetMergedNmouseHandleEPNS0_12NmouseHandleE",
            get_merged_handle as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3hid16GetNmouseTypeSetEPNS_4util10BitFlagSetILi32ENS0_10NmouseTypeEEENS0_12NmouseHandleE",
            get_type_set as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3hid15GetNmouseStatesEPiPNS0_11NmouseStateEiNS0_12NmouseHandleE",
            get_states as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3hid19EnableNmousePointerENS0_12NmouseHandleEb",
            enable_pointer as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3hid24SetNmousePointerPositionENS0_12NmouseHandleEii",
            set_pointer_position as *const () as usize,
        ),
        Export::new(
            "_ZN2nn3hid13GetJcmsHandleERKjRKNS0_8JcmsTypeE",
            get_jcms_handle as *const () as usize,
        ),
    ];
    v.extend(stub_list());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;

    #[test]
    fn the_pointer_range_is_the_one_set() {
        let (base, _guard) = guest_memory();
        set_pointer_ranges(1920, 1080);
        get_pointer_ranges(base, base + 4);
        assert_eq!(guest::read_u32(base).unwrap_or(0), 1920);
        assert_eq!(guest::read_u32(base + 4).unwrap_or(0), 1080);
    }

    #[test]
    fn only_the_merged_handle_takes_settings() {
        assert_eq!(enable_pointer(0, 1), 1);
        assert_eq!(enable_pointer(7, 1), 0);
        assert_eq!(set_pointer_position(0, 5, 5), 1);
    }

    #[test]
    fn a_jcms_handle_is_the_id_and_the_type() {
        let (base, _guard) = guest_memory();
        guest::write_u32(base, 0x102).ok();
        guest::write_u32(base + 4, 3).ok();
        assert_eq!(get_jcms_handle(base, base + 4), 0x302);
    }

    #[test]
    fn there_are_no_nmouse_states() {
        let (base, _guard) = guest_memory();
        guest::fill(base, 16, 0xff).ok();
        assert_eq!(get_states(base, base + 16, 4, 0), 1);
        assert_eq!(guest::read_u32(base).unwrap_or(1), 0);
    }
}
