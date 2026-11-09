//! The state of the application that the applet services share: operation mode, focus and the message queue.
//!
//! The host front end drives it with [`set_focus`], [`set_operation_mode`] and [`push_message`]. The applet services
//! read it. The message queue and its event follow `ICommonStateGetter` of libnx (`applet.c`) and the messages of
//! nxnative (`dm/host/hle_oe.c`): 15 for a focus change, 30 for an operation mode change and 31 for a performance
//! mode change.

use crate::config;
use crate::util::{Event, lock};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

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
    /// The focus state of the front end.
    pub(crate) focus: Focus,
    /// The focus state that the game sees: the state of the front end, or out of focus while a library applet is open
    /// over a game in focus.
    pub(crate) seen: Focus,
    applets: u32,
    sdk_event: Option<u64>,
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
    ready: Condvar,
    event: Event,
    /// The mirrors of the fields that a game reads on every frame. A read is one atomic load and takes no lock (R7).
    queued: AtomicU32,
    handheld: AtomicBool,
    seen: AtomicU8,
}

static STATE: OnceLock<State> = OnceLock::new();

/// The shared state. It is made from the [`config`] on first use.
pub(crate) fn state() -> &'static State {
    STATE.get_or_init(|| State {
        queued: AtomicU32::new(0),
        handheld: AtomicBool::new(config().handheld),
        seen: AtomicU8::new(Focus::InFocus as u8),
        inner: Mutex::new(Inner {
            handheld: config().handheld,
            focus: Focus::InFocus,
            seen: Focus::InFocus,
            applets: 0,
            sdk_event: None,
            // Without `SetFocusHandlingMode` the game gets focus messages, as in nxnative.
            notify_focus: true,
            notify_operation: false,
            notify_performance: false,
            auto_sleep_disabled: false,
            idle_extension: 0,
            cpu_overclock: false,
            queue: VecDeque::new(),
        }),
        ready: Condvar::new(),
        event: Event::new(),
    })
}

impl State {
    /// Reads or changes the fields.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut lock(&self.inner))
    }

    /// 0 for handheld and 1 for docked, the value of `GetOperationMode`. Lock free.
    pub(crate) fn operation_mode(&self) -> u32 {
        u32::from(!self.handheld.load(Ordering::Relaxed))
    }

    /// Whether the console is in handheld mode. Lock free.
    pub(crate) fn is_handheld(&self) -> bool {
        self.handheld.load(Ordering::Relaxed)
    }

    /// The focus state that the game sees, as the value of `GetCurrentFocusState`. Lock free.
    pub(crate) fn seen_focus(&self) -> u8 {
        self.seen.load(Ordering::Relaxed)
    }

    /// 0 for normal and 1 for boost, the value of `GetPerformanceMode`. A docked console and the `boost` option of the
    /// [`config`] both give boost. Lock free.
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
            self.queued.store(s.queue.len() as u32, Ordering::Release);
            true
        });
        if queued {
            self.ready.notify_all();
            self.event.signal();
            self.sync_sdk_event();
        }
    }

    /// The oldest queued message. An empty queue is answered with one atomic load, because a game polls on every frame.
    /// The event is cleared when the queue is empty.
    pub(crate) fn pop(&self) -> Option<u32> {
        if self.queued.load(Ordering::Acquire) == 0 {
            return None;
        }
        let (message, empty) = self.with(|s| {
            let m = s.queue.pop_front();
            self.queued.store(s.queue.len() as u32, Ordering::Release);
            (m, s.queue.is_empty())
        });
        if empty {
            self.event.clear();
        }
        self.sync_sdk_event();
        message
    }

    /// The oldest queued message. Waits on a condition variable until one arrives, with no polling.
    pub(crate) fn pop_wait(&self) -> u32 {
        let mut g = lock(&self.inner);
        let message = loop {
            if let Some(m) = g.queue.pop_front() {
                self.queued.store(g.queue.len() as u32, Ordering::Release);
                break m;
            }
            g = self.ready.wait(g).unwrap_or_else(|e| e.into_inner());
        };
        let empty = g.queue.is_empty();
        drop(g);
        if empty {
            self.event.clear();
        }
        self.sync_sdk_event();
        message
    }

    /// The SDK event `GetNotificationMessageEvent` fills follows the queue as the IPC event does: it is signalled while
    /// a message waits and cleared when the queue is empty (nxnative `mq_ev_sync` in `dm/host/hle_oe.c`). The state is
    /// read and the event changed under the lock of the state, so a push that races a pop cannot leave the event
    /// cleared over a queued message.
    fn sync_sdk_event(&self) {
        let g = lock(&self.inner);
        let Some(ev) = g.sdk_event else {
            return;
        };
        if g.queue.is_empty() {
            crate::event::clear(ev);
        } else {
            crate::event::signal_bound(ev);
        }
    }

    /// Makes the SDK event at the guest address `ev` the notification event and brings it in step with the queue.
    pub(crate) fn bind_sdk_event(&self, ev: u64) {
        if crate::event::bind(ev, true) {
            self.with(|s| s.sdk_event = Some(ev));
            self.sync_sdk_event();
        }
    }

    /// A library applet opens (`1`) or closes (`-1`), or the focus of the front end changed (`0`). One message 15
    /// goes out for every change of the focus state that the game sees (`focus_sync` of nxnative `hle_oe.c`).
    pub(crate) fn focus_sync(&self, applets: i32) {
        let notify = self.with(|i| {
            i.applets = i.applets.saturating_add_signed(applets);
            let seen = if i.focus == Focus::InFocus && i.applets > 0 {
                Focus::OutOfFocus
            } else {
                i.focus
            };
            let changed = i.seen != seen;
            i.seen = seen;
            self.seen.store(seen as u8, Ordering::Relaxed);
            changed && i.notify_focus
        });
        if notify {
            self.push(MSG_FOCUS);
        }
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
    s.with(|i| i.focus = focus);
    s.focus_sync(0);
}

/// Switches between handheld mode (`true`) and docked mode (`false`). The messages of the change are queued for the
/// notifications that the game enabled.
pub fn set_operation_mode(handheld: bool) {
    let s = state();
    let (operation, performance) = s.with(|i| {
        let changed = i.handheld != handheld;
        i.handheld = handheld;
        s.handheld.store(handheld, Ordering::Relaxed);
        (
            changed && i.notify_operation,
            changed && i.notify_performance,
        )
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
