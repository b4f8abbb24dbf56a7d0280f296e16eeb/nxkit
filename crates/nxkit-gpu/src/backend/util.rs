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

type Sender = std::sync::mpsc::Sender<Box<dyn Send>>;

/// Drops values on a thread of its own, which starts with the first value. The detail of a frame holds thousands of
/// draws. Freeing them took about a millisecond of the game's render thread at the end of every frame.
#[derive(Default)]
pub(super) struct Dropper {
    tx: OnceLock<Option<Mutex<Sender>>>,
}

impl Dropper {
    /// Drops `v` on the thread, or here when the thread cannot start.
    pub(super) fn drop_later<T: Send + 'static>(&self, v: T) {
        let tx = self.tx.get_or_init(|| {
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

impl GpuBackend {
    /// The walk of a submit, for the caller to run (`Backend::submit`). The walk may run later (the helper thread, or a
    /// held submit from the gate pump): the draws take the device origin of the moment the game submitted, not that of
    /// a later `device_modes`.
    pub(super) fn walk_later<'a>(
        &'a self,
        queue: u64,
        origin_lower_left: bool,
        walk: impl FnOnce(&mut Walker, &mut GuestView) + Send + 'a,
    ) -> Option<Deferred<'a>> {
        let device = self.device_lower_left.load(Ordering::Acquire);
        Some(Box::new(move || {
            let mut walker = locked(&self.walker);
            walker.device_origin(device);
            walker.queue_origin(queue, origin_lower_left);
            walk(&mut walker, &mut GuestView(self.guest.clone()));
        }))
    }
}
