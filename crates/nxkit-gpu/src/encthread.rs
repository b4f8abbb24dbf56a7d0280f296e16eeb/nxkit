//! The encoder thread: one frame is encoded while the game walks the next one.
//!
//! The backend hands a [`Job`] (a copy of the frame, its plan, the detail and a [`Snapshot`] of the texture records)
//! to a thread of its own and returns. The channel holds every message in order: jobs, presents and texture
//! retirements, so a present always sees the frame before it and a retired texture is never dropped under an encode
//! that still names it.
//!
//! At most one job is in flight. [`Encoder::settle`] waits for it, and the backend calls it before it sends the next
//! job, so the snapshot of a frame always includes what the GPU did in the frame before. The result of a job is a
//! [`TableUpdate`]. The front end applies it with [`TableUpdate::apply`], which keeps a texture `GuestNewer` when the
//! CPU wrote it after the snapshot.
//!
//! A panic in a job is logged and marks the thread dead. The backend then encodes on the calling thread again, and a
//! wait for the lost frame fails.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use nxkit_nvn::backend::{Fence, Present};
use nxkit_nvn::frame::Frame;
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::resource::{ResourceTable, TexId};

use crate::detail::FrameDetail;
use crate::guestpools::GuestPools;
use crate::snapshot::Snapshot;

/// One frame for the encoder thread.
pub(crate) struct Job {
    /// The commands of the frame.
    pub(crate) frame: Frame,
    /// The passes of the frame.
    pub(crate) plan: PassPlan,
    /// What the commands bound.
    pub(crate) detail: FrameDetail,
    /// The texture records and handles that the frame names, as they were at the flush.
    pub(crate) snapshot: Snapshot,
    /// The memory pools at the flush. The frame resolves guest addresses against them, even if the game finalizes a
    /// pool before the thread runs the job.
    pub(crate) pools: Arc<GuestPools>,
    /// The fence of the flush.
    pub(crate) fence: Fence,
}

/// What the front end changes in the texture table after a frame was encoded. Each id carries the
/// [`write_epoch`](nxkit_nvn::resource::TextureRecord::write_epoch) of its snapshot.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TableUpdate {
    /// The textures that the GPU wrote.
    pub(crate) wrote: Vec<(TexId, u32)>,
    /// The textures whose two copies hold the same data again.
    pub(crate) synced: Vec<(TexId, u32)>,
}

impl TableUpdate {
    /// Applies the update to the table of the front end. A texture that is gone or that a CPU write hit since the
    /// snapshot stays as it is.
    pub(crate) fn apply(&self, table: &mut ResourceTable) {
        for (id, epoch) in &self.wrote {
            let _ = table.gpu_wrote_at(*id, *epoch);
        }
        for (id, epoch) in &self.synced {
            let _ = table.synced_at(*id, *epoch);
        }
    }
}

/// A message for the encoder thread. The thread handles them in the order of the sends.
pub(crate) enum Msg {
    /// Encode a frame.
    Job(Box<Job>),
    /// The game presented a texture. Its device origin stays with this image while it waits.
    Present {
        present: Present,
        origin_lower_left: bool,
    },
    /// A texture was created or finalised, so its Metal texture is stale.
    Retire(TexId),
}

/// What a handler answers to a message.
pub(crate) enum Reply {
    /// The message needs no answer.
    Done,
    /// The result of a [`Msg::Job`].
    Job(Result<TableUpdate, String>),
    /// The thread ends, because nothing is left to serve.
    Stop,
}

/// What [`Encoder::settle`] found.
#[derive(Debug, Default)]
pub(crate) struct Settled {
    /// The update of the last job that finished, when nobody applied it yet.
    pub(crate) update: Option<TableUpdate>,
    /// The error of a job that failed, once.
    pub(crate) error: Option<String>,
}

#[derive(Default)]
struct State {
    sent: u64,
    finished: u64,
    update: Option<TableUpdate>,
    error: Option<String>,
    dead: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn finish(&self, result: Result<TableUpdate, String>) {
        let mut s = self.lock();
        s.finished += 1;
        match result {
            Ok(update) => s.update = Some(update),
            Err(e) => {
                eprintln!("nxkit-gpu: the encoder thread failed a frame: {e}");
                s.error = Some(e);
            }
        }
        self.cv.notify_all();
    }

    fn die(&self, why: String) {
        let mut s = self.lock();
        s.finished += 1;
        s.dead = true;
        s.error = Some(why);
        self.cv.notify_all();
    }
}

/// The handle of the encoder thread.
pub(crate) struct Encoder {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
}

fn panic_text(p: &(dyn Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

fn run(rx: Receiver<Msg>, shared: Arc<Shared>, mut handler: impl FnMut(Msg) -> Reply) {
    // The page tracker counts the accesses of this thread to tracked pages apart from the game's.
    let _internal = nxkit_metal::mark_internal_thread();
    for msg in rx {
        let is_job = matches!(msg, Msg::Job(_));
        match catch_unwind(AssertUnwindSafe(|| handler(msg))) {
            Ok(Reply::Stop) => break,
            Ok(Reply::Job(result)) => shared.finish(result),
            Ok(Reply::Done) => {}
            Err(p) => {
                let why = format!("the encoder thread panicked: {}", panic_text(p.as_ref()));
                eprintln!(
                    "nxkit-gpu: {why}; the frames are encoded on the game thread from now on"
                );
                if is_job {
                    shared.die(why);
                }
            }
        }
    }
}

impl Encoder {
    /// Starts the thread. `handler` serves each message on it.
    pub(crate) fn spawn(
        handler: impl FnMut(Msg) -> Reply + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("nxkit-enc".into())
            .stack_size(8 << 20)
            .spawn(move || run(rx, thread_shared, handler))?;
        Ok(Self { tx, shared })
    }

    /// True when a job panicked. The thread encodes no more frames then.
    pub(crate) fn is_dead(&self) -> bool {
        self.shared.lock().dead
    }

    /// True when a job failed and nobody has taken the error.
    pub(crate) fn has_error(&self) -> bool {
        self.shared.lock().error.is_some()
    }

    /// Queues a message that is not a job. False when the thread is gone, and the caller serves the message itself.
    pub(crate) fn send(&self, msg: Msg) -> bool {
        self.tx.send(msg).is_ok()
    }

    /// Queues a frame. The caller settled the job before. False when the thread is gone, and the thread is dead from now
    /// on.
    pub(crate) fn submit(&self, job: Box<Job>) -> bool {
        self.shared.lock().sent += 1;
        if self.tx.send(Msg::Job(job)).is_ok() {
            return true;
        }
        self.shared.die("the encoder thread is gone".into());
        false
    }

    /// Waits until no job is in flight, then takes the update and the error of the jobs that finished.
    pub(crate) fn settle(&self) -> Settled {
        let mut s = self.shared.lock();
        while s.finished < s.sent {
            s = self
                .shared
                .cv
                .wait(s)
                .unwrap_or_else(PoisonError::into_inner);
        }
        Settled {
            update: s.update.take(),
            error: s.error.take(),
        }
    }

    /// Waits until no job is in flight.
    pub(crate) fn wait_idle(&self) {
        let mut s = self.shared.lock();
        while s.finished < s.sent {
            s = self
                .shared
                .cv
                .wait(s)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Waits until `reached` holds. False when the thread is dead or idle and `reached` still does not hold, which
    /// means that the work for it was lost.
    pub(crate) fn wait_for(&self, reached: impl Fn() -> bool) -> bool {
        let mut s = self.shared.lock();
        loop {
            if reached() {
                return true;
            }
            if s.dead || s.finished >= s.sent {
                return false;
            }
            s = self
                .shared
                .cv
                .wait(s)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn job(fence: u64) -> Box<Job> {
        Box::new(Job {
            frame: Frame::new(),
            plan: PassPlan::default(),
            detail: FrameDetail::default(),
            snapshot: Snapshot::default(),
            pools: Arc::new(GuestPools::empty()),
            fence: Fence(fence),
        })
    }

    #[test]
    fn settle_waits_for_the_job_and_hands_out_its_update_once() {
        let done = Arc::new(AtomicU64::new(0));
        let seen = done.clone();
        let enc = Encoder::spawn(move |msg| match msg {
            Msg::Job(j) => {
                seen.store(j.fence.0, Ordering::Release);
                Reply::Job(Ok(TableUpdate {
                    wrote: vec![(TexId(1), 0)],
                    synced: Vec::new(),
                }))
            }
            _ => Reply::Done,
        })
        .expect("thread");
        assert!(enc.submit(job(3)));
        let first = enc.settle();
        assert_eq!(done.load(Ordering::Acquire), 3);
        assert_eq!(first.update.expect("update").wrote, [(TexId(1), 0)]);
        assert!(enc.settle().update.is_none());
        assert!(enc.wait_for(|| done.load(Ordering::Acquire) >= 3));
        assert!(!enc.wait_for(|| done.load(Ordering::Acquire) >= 4));
    }

    #[test]
    fn queued_presents_keep_the_origin_captured_before_a_device_mode_change() {
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;

        let (release, wait) = channel();
        let (sent, received) = channel();
        let enc = Encoder::spawn(move |msg| {
            if let Msg::Present {
                present,
                origin_lower_left,
            } = msg
            {
                wait.recv().unwrap();
                sent.send((present.texture, origin_lower_left)).unwrap();
            }
            Reply::Done
        })
        .unwrap();
        let mode = AtomicBool::new(true);
        let present = |id| Present {
            window: 1,
            texture: TexId(id),
            interval: 1,
            crop: nxkit_nvn::backend::Rect::default(),
        };
        assert!(enc.send(Msg::Present {
            present: present(1),
            origin_lower_left: mode.load(Ordering::Acquire),
        }));
        mode.store(false, Ordering::Release);
        assert!(enc.send(Msg::Present {
            present: present(2),
            origin_lower_left: mode.load(Ordering::Acquire),
        }));
        // Neither image can reach the display path until after the mode changed.
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            (TexId(1), true)
        );
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            (TexId(2), false)
        );
    }

    #[test]
    fn a_panic_in_a_job_kills_the_thread_and_fails_the_wait() {
        let enc = Encoder::spawn(|msg| match msg {
            Msg::Job(_) => panic!("boom"),
            _ => Reply::Done,
        })
        .expect("thread");
        assert!(enc.submit(job(1)));
        let s = enc.settle();
        assert!(s.error.expect("error").contains("boom"));
        assert!(enc.is_dead());
        assert!(!enc.wait_for(|| false));
    }

    #[test]
    fn a_failed_job_reports_its_error_once() {
        let enc = Encoder::spawn(|msg| match msg {
            Msg::Job(_) => Reply::Job(Err("no memory".into())),
            _ => Reply::Done,
        })
        .expect("thread");
        assert!(enc.submit(job(1)));
        let first = enc.settle();
        assert_eq!(first.error.as_deref(), Some("no memory"));
        assert!(!enc.has_error());
        assert!(enc.settle().error.is_none());
        assert!(!enc.is_dead());
    }
}
