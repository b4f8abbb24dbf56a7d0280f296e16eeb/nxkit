//! SystemEvents of the service layer (nxnative `dm/host/hle_thread.c`, `hle_bind_system_event` and its neighbours).
//!
//! A real service gives the client the read end of a kernel event, and the guest waits on it through the SDK's
//! `nn::os::SystemEventType`. The host binds such an object to a kernel event: on a guest thread it calls the SDK's
//! own `CreateSystemEvent(event, clear mode, inter-process = true)`, which makes the kernel event through svc
//! `CreateEvent`, and it records the write end handle. After that any thread, the audio thread for one, can signal
//! the event with [`Kernel::signal_system_event`].
//!
//! A binding has an id that survives neither the guest address nor the handle number being used again. The SDK's
//! `DestroySystemEvent` closes the write end, and `close_handle` then drops the binding. A signal by a stale binding
//! is refused because a handle carries a generation.
//!
//! The SDK functions come from [`crate::guest_sym`]. Binding needs SDK code, so it runs on a guest thread only.

use crate::guestsym::require;
use crate::handle::KObject;
use crate::kernel::{Kernel, current_thread};
use crate::result::{KResult, KResultOr};
use crate::thread::lock;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const CREATE: &str = "_ZN2nn2os17CreateSystemEventEPNS0_15SystemEventTypeENS0_14EventClearModeEb";
const WRITABLE: &str = "_ZN2nn2os30GetWritableHandleOfSystemEventEPKNS0_15SystemEventTypeE";

/// The most bindings at one time, as in nxnative.
const MAX_BINDINGS: usize = 512;

struct Binding {
    event: u64,
    writable: u32,
    id: u64,
}

/// The binding table of a kernel.
#[derive(Default)]
pub(crate) struct SystemEvents {
    table: Mutex<Vec<Binding>>,
    next_id: AtomicU64,
    skipped_logged: AtomicBool,
}

impl SystemEvents {
    fn find(&self, event: u64) -> Option<(u32, u64)> {
        lock(&self.table)
            .iter()
            .find(|b| b.event == event)
            .map(|b| (b.writable, b.id))
    }

    /// Drops the binding of a closed write end.
    pub(crate) fn unbind(&self, handle: u32) {
        let mut table = lock(&self.table);
        if !table.is_empty() {
            table.retain(|b| b.writable != handle);
        }
    }
}

impl Kernel {
    /// Binds the SDK `SystemEventType` at guest address `event` to a kernel event, as a manual or auto clear
    /// event. An event that is bound already is left as it is. Runs on a guest thread only.
    pub fn bind_system_event(&self, event: u64, auto_clear: bool) -> Result<(), String> {
        if self.system_events.find(event).is_some() {
            return Ok(());
        }
        if current_thread().is_none() {
            return Err("a SystemEvent is bound on a guest thread only".to_string());
        }
        let create = require(CREATE)?;
        let writable = require(WRITABLE)?;
        let rc = create.call_create_system_event(event, i32::from(auto_clear), 1);
        if rc != 0 {
            return Err(format!("nn::os::CreateSystemEvent failed with {rc:#x}"));
        }
        let handle = writable.call_u32_1(event);
        let mut table = lock(&self.system_events.table);
        if table.len() >= MAX_BINDINGS {
            return Err(format!(
                "the SystemEvent binding table is full ({MAX_BINDINGS})"
            ));
        }
        let id = self.system_events.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        table.push(Binding {
            event,
            writable: handle,
            id,
        });
        Ok(())
    }

    /// The identity of the binding of `event`, or 0 when it is not bound.
    pub fn system_event_binding(&self, event: u64) -> u64 {
        self.system_events.find(event).map_or(0, |(_, id)| id)
    }

    /// Signals the event of a binding from any thread. False when the binding is gone (the game destroyed the event
    /// or the handle is not valid any more).
    pub fn signal_system_event_binding(&self, id: u64) -> bool {
        let writable = lock(&self.system_events.table)
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.writable);
        writable.is_some_and(|w| self.signal_event(w).is_ok())
    }

    /// Signals `event` only when it is bound, from any thread. False when it is not bound.
    pub fn signal_bound_system_event(&self, event: u64) -> bool {
        self.system_events
            .find(event)
            .is_some_and(|(w, _)| self.signal_event(w).is_ok())
    }

    /// Signals `event`. An event that is not bound is bound first, as a manual clear event. Off a guest thread
    /// that is not possible, so the signal is skipped with one log line: the game has destroyed the event, as when
    /// a host thread signals late.
    pub fn signal_system_event(&self, event: u64) -> Result<(), String> {
        for bound_now in [false, true] {
            if let Some((w, _)) = self.system_events.find(event) {
                return self.signal_event(w).map_err(|e| {
                    format!("the write end {w:#x} of the SystemEvent is not valid: {e:?}")
                });
            }
            if bound_now {
                break;
            }
            if current_thread().is_none() {
                if !self
                    .system_events
                    .skipped_logged
                    .swap(true, Ordering::Relaxed)
                {
                    (self.config.log)(&format!(
                        "SystemEvent {event:#x} is signalled off a guest thread after the game destroyed it: skipped"
                    ));
                }
                return Ok(());
            }
            self.bind_system_event(event, false)?;
        }
        Err(format!(
            "SystemEvent {event:#x} is still not bound after binding"
        ))
    }

    /// Clears a bound SystemEvent, as a service does when its condition ends. An unbound one has nothing to clear.
    pub fn clear_system_event(&self, event: u64) -> Result<(), String> {
        match self.system_events.find(event) {
            Some((w, _)) => self.clear_event(w).map_err(|e| {
                format!("the write end {w:#x} of the SystemEvent is not valid: {e:?}")
            }),
            None => Ok(()),
        }
    }

    /// A second read end of the same kernel event, as a service's handle that is copied to its client. Each end
    /// closes on its own. A handle that is not a read end is an error.
    pub fn duplicate_readable_handle(&self, handle: u32) -> KResultOr<u32> {
        match self.handles.get(handle) {
            Some(KObject::EventRead(e)) => self.handles.insert(KObject::EventRead(e)),
            _ => Err(KResult::INVALID_HANDLE),
        }
    }
}
