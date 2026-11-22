//! The Metal GPU trace of the next frames, for Xcode (`gputrace`).
//!
//! The capture covers the device, so it holds every command buffer of every thread of the frame. It starts at the
//! start of the encode of the first frame, before any command buffer exists, and stops after the submit of the last
//! frame and the wait for the GPU. While it runs, the passes get labels (`diag::gpu_labels`).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use nxkit_metal::Context;

use super::{FLAGS, TRACE, set};

struct Job {
    left: u32,
    started: bool,
    path: PathBuf,
    tx: Sender<Result<PathBuf, String>>,
}

/// The one request of a trace, written by the control thread and read by the encoder (W33, the opt-in analyses).
static JOB: Mutex<Option<Job>> = Mutex::new(None);

fn job() -> std::sync::MutexGuard<'static, Option<Job>> {
    JOB.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What [`gpu_trace`] gives back.
pub struct TraceTicket {
    rx: Receiver<Result<PathBuf, String>>,
}

/// The default document: `<diag_dir>/frame-<n>-<unix seconds>.gputrace`.
fn default_path(frame: u64) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    crate::diag::diag_dir().join(format!("frame-{frame}-{secs}.gputrace"))
}

/// Asks for a GPU trace of the next `frames` frames (at least 1). It fails at once when the process cannot capture.
pub fn gpu_trace(frames: u32, path: Option<PathBuf>) -> Result<TraceTicket, String> {
    if !nxkit_metal::gpu_trace_supported() {
        return Err(
            "GPU trace capture is not supported in this process: Metal makes captures only when the \
             process started with MTL_CAPTURE_ENABLED=1 (start the game with NXKIT_GPU_CAPTURE=1)"
                .to_owned(),
        );
    }
    let path = match path {
        Some(p) => std::path::absolute(&p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => default_path(crate::diag::frame() + 1),
    };
    if path.exists() {
        return Err(format!("{} exists already", path.display()));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let (tx, rx) = channel();
    let mut j = job();
    if j.is_some() {
        return Err("another GPU trace waits or runs".to_owned());
    }
    *j = Some(Job {
        left: frames.max(1),
        started: false,
        path,
        tx,
    });
    set(TRACE, true);
    Ok(TraceTicket { rx })
}

impl TraceTicket {
    /// Waits until the trace is written. The answer is the path of the document. A trace that has not started after
    /// `timeout` is cancelled.
    pub fn wait(self, timeout: Duration) -> Result<Vec<String>, String> {
        let got = match self.rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => {
                let mut j = job();
                if j.as_ref().is_some_and(|x| !x.started) {
                    *j = None;
                    set(TRACE, false);
                    return Err(format!("no frame was encoded in {} s", timeout.as_secs()));
                }
                drop(j);
                self.rx
                    .recv_timeout(Duration::from_secs(120))
                    .map_err(|_| "the trace did not finish".to_owned())?
            }
            Err(RecvTimeoutError::Disconnected) => return Err("the trace was dropped".to_owned()),
        };
        let path = got?;
        Ok(vec![
            path.display().to_string(),
            "open it in Xcode (Pixel History, the other GPU tools)".to_owned(),
        ])
    }
}

/// Starts the capture at the start of a frame when one is asked for. True while a trace covers this frame.
pub(crate) fn trace_begin(ctx: &Context) -> bool {
    if FLAGS.load(Ordering::Relaxed) & TRACE == 0 {
        return false;
    }
    let mut j = job();
    let Some(t) = j.as_mut() else {
        set(TRACE, false);
        return false;
    };
    if !t.started {
        if let Err(e) = nxkit_metal::gpu_trace_start(ctx, &t.path) {
            let _ = t.tx.send(Err(e));
            *j = None;
            set(TRACE, false);
            return false;
        }
        t.started = true;
        crate::diag::set_tracing(true);
    }
    true
}

/// Ends a frame of a trace. `wait` waits for the GPU. After the last frame the capture stops and the path goes back.
pub(crate) fn trace_end(wait: impl FnOnce()) {
    let mut j = job();
    let Some(t) = j.as_mut() else { return };
    t.left = t.left.saturating_sub(1);
    if t.left > 0 {
        return;
    }
    wait();
    nxkit_metal::gpu_trace_stop();
    crate::diag::set_tracing(false);
    if let Some(t) = j.take() {
        let _ = t.tx.send(Ok(t.path));
    }
    set(TRACE, false);
}
