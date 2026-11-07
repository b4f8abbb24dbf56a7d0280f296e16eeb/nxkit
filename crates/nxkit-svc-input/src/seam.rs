//! Functions that the host provides to the input service.
//!
//! A guest `SystemEventType` is an object of the SDK, so only the host can bind and signal one. The host sets the
//! hooks once, before the game starts.

use std::sync::OnceLock;

/// What the host does with a guest system event.
#[derive(Clone, Copy, Debug)]
pub struct SystemEventHooks {
    /// Binds the `SystemEventType` at the guest address `event` to a new kernel event, as the SDK's
    /// `nn::os::CreateSystemEvent` does. `auto_clear` is true for `EventClearMode_AutoClear`. The input service calls
    /// it on the guest thread that called `BindNpadStyleSetUpdateEvent`, because the host runs SDK code to do it.
    pub bind: fn(event: u64, auto_clear: bool),
    /// Signals the bound event. It may be called on any thread.
    pub signal: fn(event: u64),
}

static HOOKS: OnceLock<SystemEventHooks> = OnceLock::new();

/// Sets the system event hooks. Only the first call counts.
pub fn set_system_event_hooks(hooks: SystemEventHooks) {
    let _ = HOOKS.set(hooks);
}

pub(crate) fn bind(event: u64, auto_clear: bool) {
    if let Some(h) = HOOKS.get() {
        (h.bind)(event, auto_clear);
    }
}

pub(crate) fn signal(event: u64) {
    if let Some(h) = HOOKS.get() {
        (h.signal)(event);
    }
}
