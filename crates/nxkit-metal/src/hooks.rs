//! The hooks of frame pacing ([`FrameHooks`]) and what a display tick hands them ([`DisplayTick`]). The frames call the
//! hooks and the pacer implements them, so they sit below both.

use crate::drawable::Drawable;
use crate::metrics::CommandBufferStats;

/// Hooks for frame pacing. The backend calls them. It does not pace by itself.
///
/// `frame_complete` runs on a Metal thread. Keep it short.
pub trait FrameHooks: Send + Sync {
    /// A frame starts encoding, after its slot is free.
    fn frame_begin(&self, _frame: u64) {}
    /// A frame is about to be presented.
    fn before_present(&self, _frame: u64) {}
    /// The GPU finished a frame.
    fn frame_complete(&self, _stats: &CommandBufferStats) {}
    /// The display asks for a frame. It runs on the main thread.
    ///
    /// The drawable is the one the layer reserved for this tick. Render into it and present it with `Frame::present`.
    fn display_tick(&self, _tick: DisplayTick) {}
}

/// What a display tick carries.
pub struct DisplayTick {
    /// The drawable for this tick.
    pub drawable: Drawable,
    /// The time the display wants the frame to be ready by, in seconds on the `CACurrentMediaTime` clock.
    pub target_timestamp: f64,
    /// The time the frame is shown, in seconds on the same clock.
    pub target_presentation_timestamp: f64,
    /// True in the immediate mode of the pacer: the frame is shown as soon as it is ready, so the times above are not
    /// a target (a parked drawable can be some milliseconds old). Present with `Frame::present`, not `Frame::present_at`.
    pub immediate: bool,
}
