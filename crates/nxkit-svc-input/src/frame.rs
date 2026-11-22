//! The count of frames that the game presented, for the input timeline.

use std::sync::atomic::{AtomicU64, Ordering};

// Process-wide (W26): the layer chain of the runtime counts the presents of the game and has no context of the input
// service. The service reads the count at the calls of the game and on the control socket.
static FRAME: AtomicU64 = AtomicU64::new(0);

/// A frame of the game was presented. The runtime calls it once for each present.
pub fn frame_presented() {
    FRAME.fetch_add(1, Ordering::Relaxed);
}

/// The count of frames presented since the start of the process.
pub fn frames() -> u64 {
    FRAME.load(Ordering::Relaxed)
}
