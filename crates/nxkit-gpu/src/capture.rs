//! The GPU capture trigger. While the file `/tmp/fmap/capture` exists, the next encoded frame is recorded into a
//! `.gputrace` document under `/tmp/fmap` that Xcode opens. The trigger file is removed when the capture starts, so
//! one touch captures one frame. The process needs `MTL_CAPTURE_ENABLED=1`.

use std::sync::atomic::{AtomicU32, Ordering};

use nxkit_metal::Context;

/// The file whose presence asks for a capture of the next frame.
const TRIGGER: &str = "/tmp/fmap/capture";

/// Counts the captures, so that every document gets its own name.
static COUNT: AtomicU32 = AtomicU32::new(0);

/// Starts a capture when the trigger file exists. True when a capture is running and [`end`] has to follow.
pub(crate) fn begin(ctx: &Context) -> bool {
    if !std::path::Path::new(TRIGGER).exists() {
        return false;
    }
    let _ = std::fs::remove_file(TRIGGER);
    let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let path = format!("/tmp/fmap/frame-{n}.gputrace");
    let _ = std::fs::remove_dir_all(&path);
    match ctx.capture_start(&path) {
        Ok(()) => {
            eprintln!("nxkit-gpu: capturing the next frame into {path}");
            true
        }
        Err(e) => {
            eprintln!("nxkit-gpu: GPU capture did not start: {e}");
            false
        }
    }
}

/// Ends the capture that [`begin`] started.
pub(crate) fn end(ctx: &Context) {
    ctx.capture_stop();
    eprintln!("nxkit-gpu: GPU capture written");
}
