//! The points of a recording that matter beyond its commands: the render conditions, the reports and the waits and
//! signals between queues.
//!
//! The decoder keeps them in [`CmdStream::marks`](crate::frame::CmdStream::marks). A mark sits between two commands:
//! `at` is the index of the first command after it. The front end reads the render conditions and reports when it
//! appends a stream to the frame (`crate::rendercond`), and the host reads the waits and signals to order the queues
//! before it submits.

/// `NVN_CONDITIONAL_RENDER_MODE_RENDER_IF_EQUAL`.
pub const RENDER_IF_EQUAL: u32 = 0;
/// `NVN_CONDITIONAL_RENDER_MODE_RENDER_IF_NOT_EQUAL`.
pub const RENDER_IF_NOT_EQUAL: u32 = 1;

/// `NVN_EVENT_WAIT_MODE_EQUAL`: the wait ends when the counter equals the value.
pub const EVENT_WAIT_EQUAL: u32 = 0;
/// `NVN_EVENT_WAIT_MODE_GEQUAL_WRAP`: the wait ends when the counter is at or past the value, with wrap around.
pub const EVENT_WAIT_GEQUAL_WRAP: u32 = 1;
/// `NVN_EVENT_SIGNAL_MODE_ADD`. Every other mode writes the value.
pub const EVENT_SIGNAL_ADD: u32 = 1;

/// What happens at a mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkOp {
    /// `SetRenderEnable`: the commands after it run (true) or not.
    RenderEnable(bool),
    /// `SetRenderEnableConditional`: the commands after it run when the 64-bit counters at `addr` and `addr + 16`
    /// are equal ([`RENDER_IF_EQUAL`]) or not ([`RENDER_IF_NOT_EQUAL`]).
    RenderIf {
        /// The `NVNconditionalRenderMode`.
        mode: u32,
        /// The GPU address of the first of the two 16-byte counter reports.
        addr: u64,
    },
    /// `ReportValue`: the GPU writes `value` as the counter of the 16-byte report at `addr`.
    ReportValue {
        /// The GPU address of the report.
        addr: u64,
        /// The value.
        value: u32,
    },
    /// `ResetCounter` of the `NVNcounterType` `kind`: the counter of the queue starts again at 0.
    ResetCounter {
        /// The counter type.
        kind: u32,
    },
    /// `ReportCounter` of the `NVNcounterType` `kind`: the GPU writes the counter and its time into the 16-byte
    /// `NVNcounterData` at `addr`.
    ReportCounter {
        /// The GPU address of the report.
        addr: u64,
        /// The counter type.
        kind: u32,
    },
    /// `FenceSync` in a recording. `sync` is the guest address of the `NVNsync`.
    FenceSync {
        /// The sync object.
        sync: u64,
    },
    /// `WaitSync` in a recording.
    WaitSync {
        /// The sync object.
        sync: u64,
    },
    /// `SignalEvent`. `event` is the guest address of the `NVNevent`.
    SignalEvent {
        /// The event object.
        event: u64,
        /// The `NVNeventSignalMode`.
        mode: u32,
        /// The value.
        value: u32,
    },
    /// `WaitEvent`.
    WaitEvent {
        /// The event object.
        event: u64,
        /// The `NVNeventWaitMode`.
        mode: u32,
        /// The value.
        value: u32,
    },
}

impl MarkOp {
    /// True for the waits and signals that order queues.
    pub fn orders_queues(&self) -> bool {
        matches!(
            self,
            Self::FenceSync { .. }
                | Self::WaitSync { .. }
                | Self::SignalEvent { .. }
                | Self::WaitEvent { .. }
        )
    }
}

/// One mark of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The index of the first command after the mark.
    pub at: u32,
    /// What happens.
    pub op: MarkOp,
}

/// Whether an event counter at `current` ends a wait of `mode` for `value`. An unknown mode ends it, so the host never
/// holds work for a wait that it does not understand.
pub fn event_wait_done(mode: u32, current: u32, value: u32) -> bool {
    match mode {
        EVENT_WAIT_EQUAL => current == value,
        EVENT_WAIT_GEQUAL_WRAP => current.wrapping_sub(value) as i32 >= 0,
        _ => true,
    }
}

/// The counter of an event after a signal of `mode` with `value`.
pub fn event_signalled(mode: u32, old: u32, value: u32) -> u32 {
    if mode == EVENT_SIGNAL_ADD {
        old.wrapping_add(value)
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_wait_compares_with_wrap_around() {
        assert!(event_wait_done(EVENT_WAIT_EQUAL, 5, 5));
        assert!(!event_wait_done(EVENT_WAIT_EQUAL, 6, 5));
        assert!(event_wait_done(EVENT_WAIT_GEQUAL_WRAP, 6, 5));
        assert!(!event_wait_done(EVENT_WAIT_GEQUAL_WRAP, 4, 5));
        assert!(event_wait_done(EVENT_WAIT_GEQUAL_WRAP, 1, u32::MAX));
        assert!(event_wait_done(7, 0, 5));
    }

    #[test]
    fn an_add_signal_adds_and_the_others_write() {
        assert_eq!(event_signalled(EVENT_SIGNAL_ADD, u32::MAX, 2), 1);
        assert_eq!(event_signalled(0, 9, 2), 2);
    }

    #[test]
    fn only_waits_and_signals_order_queues() {
        assert!(
            MarkOp::WaitEvent {
                event: 1,
                mode: 0,
                value: 0
            }
            .orders_queues()
        );
        assert!(!MarkOp::RenderEnable(false).orders_queues());
        assert!(!MarkOp::ReportValue { addr: 0, value: 0 }.orders_queues());
    }
}
