//! The distributed notifications the host posts for the launcher that started it, `org.nxkit.<what>.<pid>`: `loading`
//! with the progress of the start (`done` and `total` in the user info), `shown` when the window shows the game's first
//! picture, and `paused` / `resumed` for a game sent to the launcher (`homeui.rs`). A launcher listens for the process it
//! opened.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSDistributedNotificationCenter, NSNumber, NSString};

/// `org.nxkit.<what>.<pid>` of this process.
pub(super) fn name(what: &str) -> Retained<NSString> {
    NSString::from_str(&format!("org.nxkit.{what}.{}", std::process::id()))
}

/// Posts `org.nxkit.<what>.<pid>`.
pub(super) fn post(what: &str) {
    let center = NSDistributedNotificationCenter::defaultCenter();
    // SAFETY: a distributed notification with a name and no object or user info.
    unsafe {
        center.postNotificationName_object_userInfo_deliverImmediately(
            &name(what),
            None,
            None,
            true,
        );
    }
}

/// Posts `org.nxkit.loading.<pid>` with `done` of `total` steps of the start.
pub(super) fn post_progress(done: usize, total: usize) {
    let keys = [NSString::from_str("done"), NSString::from_str("total")];
    let values = [
        NSNumber::new_u64(done as u64),
        NSNumber::new_u64(total as u64),
    ];
    let info: Retained<NSDictionary<NSString, NSNumber>> =
        NSDictionary::from_slices(&[&*keys[0], &*keys[1]], &[&*values[0], &*values[1]]);
    // SAFETY: the user info is a dictionary of property list values, as a distributed notification needs.
    unsafe {
        let info: &NSDictionary<AnyObject, AnyObject> = &*(Retained::as_ptr(&info).cast());
        NSDistributedNotificationCenter::defaultCenter()
            .postNotificationName_object_userInfo_deliverImmediately(
                &name("loading"),
                None,
                Some(info),
                true,
            );
    }
}
