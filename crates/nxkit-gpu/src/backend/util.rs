//! Free helpers of the backend: the fence-ordered take, the frame check, the drop thread and the diagnostic switches.

use super::*;

/// Takes the entries of the frames up to fence `upto` out of `held`.
pub(super) fn take_due<T>(held: &Mutex<Vec<(u64, T)>>, upto: u64) -> Vec<(u64, T)> {
    let mut c = locked(held);
    let (due, keep): (Vec<_>, Vec<_>) = c.drain(..).partition(|(f, _)| *f <= upto);
    *c = keep;
    due
}

/// A report whose 16 bytes are not in guest memory is not written. Said once.
pub(super) fn report_refused(addr: u64) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "nxkit-gpu: a counter report at {addr:#x} is not in guest memory, it is not written"
        );
    });
}

/// Drops `v` on a thread of its own. The detail of a frame holds thousands of draws; freeing them took about a millisecond
/// of the game's render thread at the end of every frame.
pub(super) fn drop_later<T: Send + 'static>(v: T) {
    type Sender = std::sync::mpsc::Sender<Box<dyn Send>>;
    static TX: OnceLock<Option<Mutex<Sender>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Box<dyn Send>>();
        let spawned = std::thread::Builder::new()
            .name("nxkit-drop".into())
            .spawn(move || rx.into_iter().for_each(drop));
        spawned.ok().map(|_| Mutex::new(tx))
    });
    let v: Box<dyn Send> = Box::new(v);
    match tx {
        Some(tx) => {
            if let Err(e) = locked(tx).send(v) {
                drop(e.0);
            }
        }
        None => drop(v),
    }
}

/// Whether `NXKIT_DIAG` asks for the overlap counters.
pub(super) fn overlap_diag() -> bool {
    static DIAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DIAG.get_or_init(|| crate::diag("overlap"))
}

/// Whether `NXKIT_DIAG` asks for the periodic counters.
pub(super) fn stats_diag() -> bool {
    static DIAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DIAG.get_or_init(|| crate::diag("stats"))
}

/// True when the detail has one entry for each command that needs one.
pub(super) fn matches(frame: &Frame, d: &FrameDetail) -> bool {
    let (mut draws, mut clears, mut dispatches) = (0, 0, 0);
    for c in &frame.cmds {
        match c {
            Cmd::Draw { .. } => draws += 1,
            Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => clears += 1,
            Cmd::Dispatch { .. } => dispatches += 1,
            _ => {}
        }
    }
    d.draws.len() == draws && d.clears.len() == clears && d.dispatches.len() == dispatches
}

pub(super) fn fail(e: impl std::fmt::Display) -> BackendError {
    BackendError(e.to_string())
}
