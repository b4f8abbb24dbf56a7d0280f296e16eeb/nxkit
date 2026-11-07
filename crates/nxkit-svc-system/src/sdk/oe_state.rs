//! The notification queue, the focus state and the library applet counter of `nn::oe`. Source: nxnative
//! `dm/host/hle_oe.c` (`mq_*`, `focus_sync`, `dm_applet_enter`, `dm_applet_leave`).
//!
//! The state is the one that the `appletOE` IPC services use, so a game sees the same queue through both paths.

use super::{log, put_u32};
use crate::event::Block;
use crate::state::state;
use std::sync::atomic::{AtomicU32, Ordering};

/// The size of a `nn::os::SystemEventType` that the host reserves for the notification event.
const EVENT_BYTES: u64 = 0x80;

static MESSAGE_EVENT: Block = Block::new();
static OPEN_APPLETS: AtomicU32 = AtomicU32::new(0);

/// A library applet opens over the game: the game sees the focus state out of focus while it is open. Every call is
/// paired with [`applet_leave`].
pub(super) fn applet_enter(name: &str) {
    log(&format!("oe library applet: {name}"));
    OPEN_APPLETS.fetch_add(1, Ordering::SeqCst);
    state().focus_sync(1);
}

/// The library applet closes.
pub(super) fn applet_leave() {
    if OPEN_APPLETS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_err() {
        super::fatal("applet_leave without applet_enter");
    }
    state().focus_sync(-1);
}

/// Opens a library applet for the lifetime of the guard.
pub(super) struct AppletGuard;

impl AppletGuard {
    /// Enters the applet `name`.
    pub(super) fn new(name: &str) -> AppletGuard {
        applet_enter(name);
        AppletGuard
    }
}

impl Drop for AppletGuard {
    fn drop(&mut self) {
        applet_leave();
    }
}

/// `GetCurrentFocusState`: the focus state that the game sees.
extern "C" fn current_focus() -> u32 {
    u32::from(state().seen_focus())
}

/// `SetFocusHandlingMode`: 1 and 2 get focus notifications, 0 and 3 none.
extern "C" fn set_focus_mode(mode: u32) {
    let notifies = mode != 0 && mode != 3;
    state().with(|i| i.notify_focus = notifies);
    log(&format!(
        "oe::SetFocusHandlingMode {mode} ({})",
        if notifies { "focus notifications" } else { "suspended out of focus, no focus notifications" }
    ));
}

/// `TryPopNotificationMessage(uint32_t*)`.
extern "C" fn try_pop(msg: u64) -> u32 {
    match state().pop() {
        Some(m) => {
            put_u32(msg, m);
            log(&format!("oe notification popped by the game: {m}"));
            1
        }
        None => 0,
    }
}

/// `PopNotificationMessage`: waits until a message is queued.
extern "C" fn pop() -> u32 {
    state().pop_wait()
}

/// `GetNotificationMessageEvent`: the address of the auto-clear event that follows the queue.
extern "C" fn message_event() -> u64 {
    let addr = MESSAGE_EVENT.addr(EVENT_BYTES);
    state().bind_sdk_event(addr);
    addr
}

extern "C" fn set_operation_notification(on: u32) {
    state().with(|i| i.notify_operation = on & 0xff != 0);
}

extern "C" fn set_performance_notification(on: u32) {
    state().with(|i| i.notify_performance = on & 0xff != 0);
}

table! {
    "_ZN2nn2oe20GetCurrentFocusStateEv" => current_focus,
    "_ZN2nn2oe20SetFocusHandlingModeENS0_17FocusHandlingModeE" => set_focus_mode,
    "_ZN2nn2oe25TryPopNotificationMessageEPj" => try_pop,
    "_ZN2nn2oe22PopNotificationMessageEv" => pop,
    "_ZN2nn2oe27GetNotificationMessageEventEv" => message_event,
    "_ZN2nn2oe28SetResumeNotificationEnabledEb" => super::nothing,
    "_ZN2nn2oe42SetOperationModeChangedNotificationEnabledEb" => set_operation_notification,
    "_ZN2nn2oe44SetPerformanceModeChangedNotificationEnabledEb" => set_performance_notification,
}
