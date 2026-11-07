//! The state of the application that the applet services share: operation mode, focus and the message queue.
//!
//! The host front end drives it with [`set_focus`], [`set_operation_mode`] and [`push_message`]. The applet services
//! read it. The message queue and its event follow `ICommonStateGetter` of libnx (`applet.c`) and the messages of
//! nxnative (`dm/host/hle_oe.c`): 15 for a focus change, 30 for an operation mode change and 31 for a performance
//! mode change.

use crate::config;
use crate::util::{Event, lock};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// The message of a focus change.
pub(crate) const MSG_FOCUS: u32 = 15;
/// The message of an operation mode change.
pub(crate) const MSG_OPERATION_MODE: u32 = 30;
/// The message of a performance mode change.
pub(crate) const MSG_PERFORMANCE_MODE: u32 = 31;

/// The slots of the message queue of the applet.
const QUEUE_SLOTS: usize = 32;

/// The focus state of the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The application is in front.
    InFocus = 1,
    /// A system overlay covers the application.
    OutOfFocus = 2,
    /// The application is in the background.
    Background = 3,
}

pub(crate) struct Inner {
    pub(crate) handheld: bool,
    pub(crate) focus: Focus,
    pub(crate) notify_focus: bool,
    pub(crate) notify_operation: bool,
    pub(crate) notify_performance: bool,
    pub(crate) auto_sleep_disabled: bool,
    pub(crate) idle_extension: u32,
    pub(crate) cpu_overclock: bool,
    queue: VecDeque<u32>,
}

pub(crate) struct State {
    inner: Mutex<Inner>,
    event: Event,
}

static STATE: OnceLock<State> = OnceLock::new();

/// The shared state. It is made from the [`config`] on first use.
pub(crate) fn state() -> &'static State {
    STATE.get_or_init(|| State {
        inner: Mutex::new(Inner {
            handheld: config().handheld,
            focus: Focus::InFocus,
            // Without `SetFocusHandlingMode` the game gets focus messages, as in nxnative.
            notify_focus: true,
            notify_operation: false,
            notify_performance: false,
            auto_sleep_disabled: false,
            idle_extension: 0,
            cpu_overclock: false,
            queue: VecDeque::new(),
        }),
        event: Event::new(),
    })
}

impl State {
    /// Reads or changes the fields.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut lock(&self.inner))
    }

    /// 0 for handheld and 1 for docked, the value of `GetOperationMode`.
    pub(crate) fn operation_mode(&self) -> u32 {
        u32::from(!self.with(|s| s.handheld))
    }

    /// 0 for normal and 1 for boost, the value of `GetPerformanceMode`. A docked console and the `boost` option of the
    /// [`config`] both give boost.
    pub(crate) fn performance_mode(&self) -> u32 {
        if config().boost {
            1
        } else {
            self.operation_mode()
        }
    }

    /// Queues a message. A message of a state change replaces an equal one that is still queued.
    pub(crate) fn push(&self, message: u32) {
        let queued = self.with(|s| {
            let merge = matches!(
                message,
                MSG_FOCUS | MSG_OPERATION_MODE | MSG_PERFORMANCE_MODE
            );
            if merge && s.queue.contains(&message) {
                return true;
            }
            if s.queue.len() >= QUEUE_SLOTS {
                return false;
            }
            s.queue.push_back(message);
            true
        });
        if queued {
            self.event.signal();
        }
    }

    /// The oldest queued message. The event is cleared when the queue is empty.
    pub(crate) fn pop(&self) -> Option<u32> {
        let (message, empty) = self.with(|s| (s.queue.pop_front(), s.queue.is_empty()));
        if empty {
            self.event.clear();
        }
        message
    }

    /// The readable handle of the message event. The event is signalled when messages wait.
    pub(crate) fn event_handle(&self) -> Option<u32> {
        let handle = self.event.readable()?;
        if !self.with(|s| s.queue.is_empty()) {
            self.event.signal();
        }
        Some(handle)
    }
}

/// Sets the focus state of the application. A focus message is queued when the game asked for focus notifications.
pub fn set_focus(focus: Focus) {
    let s = state();
    let notify = s.with(|i| {
        let changed = i.focus != focus;
        i.focus = focus;
        changed && i.notify_focus
    });
    if notify {
        s.push(MSG_FOCUS);
    }
}

/// Switches between handheld mode (`true`) and docked mode (`false`). The messages of the change are queued for the
/// notifications that the game enabled.
pub fn set_operation_mode(handheld: bool) {
    let s = state();
    let (operation, performance) = s.with(|i| {
        let changed = i.handheld != handheld;
        i.handheld = handheld;
        (changed && i.notify_operation, changed && i.notify_performance)
    });
    if operation {
        s.push(MSG_OPERATION_MODE);
    }
    if performance {
        s.push(MSG_PERFORMANCE_MODE);
    }
}

/// Queues an applet message of the system, for example 4 for a request to exit (`AppletMessage_ExitRequest` of libnx).
pub fn push_message(message: u32) {
    state().push(message);
}
