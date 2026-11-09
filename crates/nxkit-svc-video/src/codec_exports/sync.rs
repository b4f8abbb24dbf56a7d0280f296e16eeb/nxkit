//! The sync handles and the sync point events.

use super::*;

// ---------- sync handles and sync point events ----------

pub(super) extern "C" fn try_wait_native_sync(sync: u64) -> u32 {
    read_sync(sync).is_none_or(|s| sync_reached(&s)).into()
}

pub(super) fn event_signal(e: u64) {
    if let Some(c) = config() {
        c.host.signal_system_event(e);
    }
}

pub(super) fn event_clear(e: u64) {
    if let Some(c) = config() {
        c.host.clear_system_event(e);
    }
}

/// Where the awaited handle lies inside a sync point event of the SDK. The magic that marks the event as the host's
/// is the first word.
pub(super) const SPT_MAGIC_AT: u64 = 0x40;
/// The bytes of a sync point event that the host owns: a system event of 0x30 bytes, and the awaited handle.
pub(super) const SPT_BYTES: u64 = 0x58;

pub(super) extern "C" fn spt_create(e: u64) -> u32 {
    let Some(c) = config() else {
        return BAD;
    };
    if guest::fill(e, SPT_BYTES, 0).is_err() {
        return BAD;
    }
    c.host.bind_system_event(e);
    let _ = guest::write_u32(e + SPT_MAGIC_AT, SYNC_MAGIC);
    0
}

pub(super) extern "C" fn spt_holder(holder: u64, e: u64) {
    match crate::guest_api::os_fns() {
        Ok(f) => (f.init_multi_wait_holder)(holder, e),
        Err(err) => warn(&format!(
            "nn::syncpt::sdk::InitializeMultiWaitHolder: {err}"
        )),
    }
}

pub(super) extern "C" fn spt_request(e: u64, sync: u64) -> u32 {
    unarm(e);
    event_clear(e);
    let Some(want) = read_sync(sync) else {
        return BAD;
    };
    if !arm(e, want) {
        warn("nn::syncpt: more than 64 armed events: signalled at once");
        event_signal(e);
        return 0;
    }
    if sync_reached(&want) {
        unarm(e);
        event_signal(e);
    }
    0
}

pub(super) extern "C" fn spt_cancel(e: u64) {
    unarm(e);
    event_clear(e);
}

pub(super) extern "C" fn spt_try_wait(e: u64) -> u32 {
    match crate::guest_api::os_fns() {
        Ok(f) => u32::from((f.try_wait_system_event)(e) & 0xff != 0),
        Err(err) => {
            warn(&format!("nn::syncpt::sdk::TryWaitSyncPointEvent: {err}"));
            0
        }
    }
}

pub(super) extern "C" fn spt_destroy(e: u64) {
    unarm(e);
    match crate::guest_api::os_fns() {
        Ok(f) => (f.destroy_system_event)(e),
        Err(err) => warn(&format!("nn::syncpt::sdk::DestroySyncPointEvent: {err}")),
    }
    let _ = guest::write_u32(e + SPT_MAGIC_AT, 0);
}
