//! The feeder of the Neural Engine path: it runs the model on each picture as soon as the decoder has it, ahead of the
//! game, and keeps the results until the game's frame that shows the picture asks for them.
//!
//! The video service tells the feeder (a [`PictureSink`]) of every decoded picture and of the guest memory the game
//! read it into. A thread of its own converts the picture to the model's input on the CPU ([`super::conv`]) and starts
//! the model; when it is done the picture's result is `Ready` in [`super::slots`]. The game's NDVE dispatch names its
//! planes by their addresses: [`Feed::pick`] finds the picture they hold and, if its result is ready on the CPU's side
//! (the Core ML completion has run), hands out a [`Hold`] on the slot. Nothing on the GPU waits for the Neural Engine.
//! A picture whose result is not ready is made by the Metal plan.
//!
//! The conversion's constants (the game's colour matrix and offsets) are not known before the first NDVE dispatch; they
//! are taken from its push block, and pictures decoded before that are made by the plan.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock, PoisonError};
use std::time::Instant;

use nxkit_svc_video::{Frame, PictureSink};

use super::ane::{SLOTS, Session};
use super::conv::{self, Params};
use super::slots::{Binds, Miss, Pic, Table, Why};

/// Decoded pictures that wait for the feeder. The decoder is three ahead of the game, so a longer queue means the
/// feeder cannot keep up; the oldest pictures are dropped (the plan makes them).
const QUEUE: usize = 8;

/// The feeder's state, shared by the video service's threads, the feeder thread and the encoder.
struct State {
    table: Table,
    binds: Binds,
    queue: VecDeque<Arc<Frame>>,
    /// The conversion constants of the game and their number: it changes when they do.
    params: Option<Params>,
    epoch: u32,
    /// The model of the weights the game binds, with the key of those weights.
    session: Option<(u64, Session)>,
    /// The last failure of the conversion or of the Neural Engine, for the health line.
    error: String,
}

/// The picture feed of the process.
pub(super) struct Feed {
    state: Mutex<State>,
    wake: Condvar,
}

/// The feed. It exists once the encoder asks for it ([`install`]).
pub(super) fn feed() -> &'static Feed {
    static FEED: OnceLock<Feed> = OnceLock::new();
    FEED.get_or_init(|| Feed {
        state: Mutex::new(State {
            table: Table::new(SLOTS),
            binds: Binds::default(),
            queue: VecDeque::new(),
            params: None,
            epoch: 0,
            session: None,
            error: String::new(),
        }),
        wake: Condvar::new(),
    })
}

/// Starts the feeder thread and tells the video service to report its pictures. Once.
pub(super) fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let f = feed();
        match std::thread::Builder::new()
            .name("ane-feed".into())
            .spawn(move || f.run())
        {
            Ok(_) => nxkit_svc_video::set_picture_sink(Arc::new(Sink)),
            Err(e) => eprintln!("nxkit: the Neural Engine feeder did not start: {e}"),
        }
    });
}

struct Sink;

impl PictureSink for Sink {
    fn decoded(&self, frame: &Arc<Frame>) {
        feed().queue(frame);
    }

    fn read_nv12(&self, frame: u64, luma: u64, chroma: u64, _width: u32, _height: u32) {
        feed().lock().binds.bind(luma, chroma, frame);
    }
}

/// A GPU frame's claim on the result of a picture: the slot is kept until the frame has finished (or was dropped).
pub(super) struct Hold {
    slot: usize,
    pic: Pic,
}

impl Hold {
    /// The slot whose outputs the frame reads.
    pub(super) fn slot(&self) -> usize {
        self.slot
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        feed().lock().table.release(self.pic);
    }
}

/// Why a picture has no result: the reason of the health line and a sentence.
pub(super) type Reason = (&'static str, String);

impl Feed {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A picture was decoded: queued for the model, or, when there is no model to run yet, made by the plan.
    fn queue(&self, frame: &Arc<Frame>) {
        let mut st = self.lock();
        let pic = frame.id();
        st.table.queued(pic);
        if st.session.is_none() {
            st.table.skip(pic, Why::Warmup);
            return;
        }
        st.queue.push_back(frame.clone());
        while st.queue.len() > QUEUE {
            if let Some(old) = st.queue.pop_front() {
                st.table.skip(old.id(), Why::Busy);
            }
        }
        drop(st);
        self.wake.notify_one();
    }

    /// The model the game's weights are. A new key (other weights) starts over with the new model's slots.
    pub(super) fn attach(&self, key: u64, session: &Session) {
        let mut st = self.lock();
        if st.session.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        st.session = Some((key, session.clone()));
        st.table = Table::new(SLOTS);
        st.queue.clear();
    }

    /// The conversion constants the game's dispatch pushes. Gives their number, which changes with them.
    pub(super) fn note_params(&self, p: Params) -> u32 {
        let mut st = self.lock();
        if st.params != Some(p) {
            st.params = Some(p);
            st.epoch += 1;
        }
        st.epoch
    }

    /// The result for the picture in the planes at the host addresses `luma` and `chroma`, made with the constants
    /// `epoch`, if it is ready. Otherwise the reason it is not.
    pub(super) fn pick(&self, epoch: u32, luma: u64, chroma: u64) -> Result<Hold, Reason> {
        let mut st = self.lock();
        let Some(pic) = st.binds.find(luma, chroma) else {
            return Err((
                "unknown_picture",
                format!(
                    "no picture was read into the planes {luma:#x} and {chroma:#x}; {}",
                    st.binds.recent()
                ),
            ));
        };
        match st.table.take(pic, epoch) {
            Ok(slot) => Ok(Hold { slot, pic }),
            Err(Miss::Unknown) => Err((
                "unknown_picture",
                format!("picture {pic} was not reported by the decoder"),
            )),
            Err(Miss::Late) => Err((
                "late",
                format!("picture {pic} is still on its way through the Neural Engine"),
            )),
            Err(Miss::Params) => Err((
                "params",
                format!("picture {pic} was converted with other constants than the game binds"),
            )),
            Err(Miss::Skipped(why)) => Err(match why {
                Why::Warmup => (
                    "warmup",
                    format!(
                        "picture {pic} was decoded before the model and the constants were ready"
                    ),
                ),
                Why::Busy => (
                    "late",
                    format!(
                        "picture {pic} found no free slot, or its slot went to a newer picture"
                    ),
                ),
                Why::Size => (
                    "unknown_picture",
                    format!("picture {pic} is not the size of the model's input"),
                ),
                Why::Failed => ("error", format!("picture {pic}: {}", st.error)),
            }),
        }
    }

    /// The feeder thread.
    fn run(&'static self) {
        loop {
            let frame = {
                let mut st = self.lock();
                loop {
                    if let Some(f) = st.queue.pop_front() {
                        break f;
                    }
                    st = self.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
                }
            };
            self.feed_one(&frame);
        }
    }

    /// Writes the input of one picture and starts the model on it.
    fn feed_one(&'static self, frame: &Frame) {
        let pic = frame.id();
        let (session, params, epoch) = {
            let st = self.lock();
            (st.session.clone(), st.params, st.epoch)
        };
        let loaded = session.as_ref().and_then(|(_, s)| s.get().ok());
        let (Some(loaded), Some(params)) = (loaded, params) else {
            self.lock().table.skip(pic, Why::Warmup);
            return;
        };
        if let Err(why) = conv::check(frame) {
            let mut st = self.lock();
            st.error = why;
            st.table.skip(pic, Why::Size);
            return;
        }
        let Some(slot) = self.lock().table.start(pic, epoch) else {
            return;
        };
        let started = Instant::now();
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
        let mut made = Ok(());
        let written = loaded.slots[slot]
            .input
            .write_bytes(|bytes, pitch| {
                made = conv::convert(frame, &params, bytes, pitch, threads);
            })
            .map_err(|e| e.to_string())
            .and(made);
        if let Err(e) = written {
            self.finished(slot, pic, Some(e));
            return;
        }
        let converted = started.elapsed();
        loaded.predict(slot, move |r| {
            log_timing(pic, converted, started.elapsed());
            self.finished(slot, pic, r.err());
        });
    }

    /// The model is done with the picture on `slot`, or failed with `error`.
    fn finished(&self, slot: usize, pic: Pic, error: Option<String>) {
        let mut st = self.lock();
        if let Some(e) = &error {
            static SHOWN: AtomicU32 = AtomicU32::new(0);
            if SHOWN.fetch_add(1, Ordering::Relaxed) < 5 {
                eprintln!("nxkit: ndve.ane: picture {pic}: {e}");
            }
            st.error = e.chars().take(120).collect();
        }
        st.table.finished(slot, pic, error.is_none());
    }
}

/// The first pictures' time for the conversion and for the conversion plus the Neural Engine, once each.
fn log_timing(pic: Pic, converted: std::time::Duration, total: std::time::Duration) {
    static SHOWN: AtomicU32 = AtomicU32::new(0);
    if SHOWN.fetch_add(1, Ordering::Relaxed) < 3 {
        eprintln!(
            "nxkit: ndve.ane: picture {pic}: input written in {:.1} ms, result after {:.1} ms",
            converted.as_secs_f32() * 1e3,
            total.as_secs_f32() * 1e3
        );
    }
}
