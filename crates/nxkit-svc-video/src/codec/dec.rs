//! The methods of a decoder: submit, release, acquire and the drain that moves pictures into buffers.

use super::*;

impl Dec {
    pub(crate) fn new(id: u32, codec: Codec, event: u64) -> Dec {
        Dec {
            id,
            codec,
            event,
            submits: 0,
            pictures: 0,
            stream: None,
            slots: vec![Slot::default(); BUFS],
            fifo: VecDeque::new(),
            pend: VecDeque::new(),
            held: VecDeque::new(),
            eos_pending: false,
            errors: 0,
            refused: 0,
            open_av1: crate::backend::open_av1,
            applier: Arc::new(apply),
            me: Weak::new(),
            worker: None,
            cv: Arc::new(Condvar::new()),
            order: Arc::new(Mutex::new(())),
            queued: VecDeque::new(),
            inflight: 0,
            lag: 0,
            flush_req: false,
            event_state: None,
            refusals: 0,
            last_diag: Instant::now(),
            busy: false,
            stop: false,
        }
    }

    /// The packets and pictures that the decoder holds for the game: queued, in flight and waiting for a buffer.
    ///
    /// With a worker it is the input side only: packets queued and in flight. Its pictures are at most
    /// [`LOOKAHEAD`](super::LOOKAHEAD), because the worker waits for a released buffer before it decodes more. Counting
    /// them closed the input while the game held both its buffers, and a player that sends before it releases waited
    /// for ever (Ocarina of Time's opening: 11 pictures held, the event down, no buffer released).
    pub(super) fn load(&self) -> usize {
        if self.worker.is_some() {
            self.queued.len() + self.inflight
        } else {
            self.held.len() + self.queued.len() + self.inflight
        }
    }

    /// The load at which the event goes down. A decoder with a lag has that many frames more in flight.
    pub(super) fn ready_limit(&self) -> usize {
        (READY + self.lag).min(HOLD - 2)
    }

    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    pub(crate) fn event(&self) -> u64 {
        self.event
    }

    /// Registers buffer `i`, which is acquirable at once.
    pub(crate) fn register(&mut self, i: u32, pool: Option<PoolRef>, pidx: u32) -> u32 {
        trace(format_args!(
            "dec {} register {i} pool idx {pidx} native {}",
            self.id,
            pool.as_ref().is_some_and(|p| lock(p).native)
        ));
        let Some(s) = self.slots.get_mut(i as usize) else {
            red(&format!(
                "nn::codec: register of buffer {i}, a decoder has {BUFS}"
            ));
            return BAD;
        };
        *s = Slot {
            pool,
            pidx,
            reg: true,
            ..Slot::default()
        };
        if self.fifo.len() < FIFO {
            self.fifo.push_back(i);
        } else {
            red(&format!(
                "nn::codec: the queue of acquirable buffers is full at register {i}"
            ));
        }
        0
    }

    /// The buffer leaves the decoder: it is no longer acquirable, filled or waited for.
    pub(crate) fn unregister(&mut self, i: u32) -> u32 {
        let Some(s) = self.slots.get_mut(i as usize) else {
            return BAD;
        };
        s.reg = false;
        s.filled = false;
        s.pool = None;
        s.generation = s.generation.wrapping_add(1);
        self.fifo.retain(|&v| v != i);
        self.pend.retain(|&v| v != i);
        0
    }

    /// Stop and Flush: pictures not yet in a buffer and a pending end of stream are dropped, and the decoder starts
    /// over at the next key frame.
    pub(crate) fn reset(&mut self, fx: &mut Effects) {
        self.held.clear();
        self.queued.clear();
        self.inflight = 0;
        self.eos_pending = false;
        self.stream = None;
        if self.worker.is_some() {
            self.flush_req = true;
            self.cv.notify_all();
        }
        if self.event != 0 {
            fx.event = Some(true);
        }
    }

    /// Waits until the worker has flushed. Called by the thread that called [`Dec::reset`], with the guard of the lock.
    pub(super) fn wait_idle(guard: MutexGuard<'_, Dec>) -> MutexGuard<'_, Dec> {
        let cv = Arc::clone(&guard.cv);
        let mut d = guard;
        while d.worker.is_some() && (d.flush_req || d.busy) {
            d = cv.wait(d).unwrap_or_else(PoisonError::into_inner);
        }
        d
    }

    /// A started decoder that has room takes packets, whatever became of the signal that Initialize gave. A player
    /// can clear that one and count only later signals.
    pub(crate) fn start(&self, fx: &mut Effects) {
        if self.event != 0 && self.load() < self.ready_limit() {
            fx.event = Some(true);
        }
    }

    /// `NXKIT_DIAG=codec`: one line a second for this decoder, not capped, to see where a stream sticks.
    pub(super) fn diag_tick(&mut self, who: &str) {
        if !diag_codec() || self.last_diag.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_diag = Instant::now();
        eprintln!(
            "DIAG codec state dec {} ({who}): held {} queued {} inflight {} pend {} fifo {} event {:?} pictures {} submits {} refusals {} busy {} flush {}",
            self.id,
            self.held.len(),
            self.queued.len(),
            self.inflight,
            self.pend.len(),
            self.fifo.len(),
            self.event_state,
            self.pictures,
            self.submits,
            self.refusals,
            self.busy,
            self.flush_req
        );
    }

    pub(super) fn hold(&mut self, frame: Arc<Frame>, ts: i64) -> bool {
        if self.held.len() >= HOLD {
            if self.errors < 4 {
                self.errors += 1;
                red(&format!(
                    "nn::codec {}: a packet gave more pictures than the host holds, one is dropped",
                    self.codec.name()
                ));
            }
            return false;
        }
        self.held.push_back(Held {
            frame,
            ts,
            eos: false,
        });
        true
    }

    /// One packet: decoded at once. Its pictures wait for released buffers. The end of stream marks the last picture,
    /// or comes as a buffer of its own. Gives the number of this submission.
    pub(crate) fn submit(
        &mut self,
        data: &[u8],
        ts: i64,
        eos: bool,
        fx: &mut Effects,
    ) -> Result<u64, u32> {
        trace(format_args!(
            "dec {} submit {} bytes ts {ts} eos {eos} (held {}, pend {:?}, load {})",
            self.id,
            data.len(),
            self.held.len(),
            self.pend,
            self.load()
        ));
        self.diag_tick("submit");
        if self.load() >= HOLD {
            trace(format_args!("dec {} submit refused, busy", self.id));
            self.refusals += 1;
            if self.refused < 4 {
                self.refused += 1;
                red(&format!(
                    "nn::codec {}: a packet is refused with {} pictures and packets waiting",
                    self.codec.name(),
                    self.load()
                ));
            }
            if self.event != 0 {
                fx.event = Some(false);
            }
            return Err(BUSY);
        }
        if self.worker.is_none() {
            let mut got = false;
            if !data.is_empty() {
                match decode_big(self.codec, &mut self.stream, self.open_av1, data) {
                    Ok(Step::Picture(Some(frame))) => got = self.hold(frame, ts),
                    Ok(Step::Picture(None)) => {}
                    Ok(Step::Pipeline(pipe)) => self.start_worker(pipe),
                    Err(e) => {
                        if self.errors < 4 {
                            self.errors += 1;
                            red(&format!("nn::codec {}: {e}", self.codec.name()));
                        }
                    }
                }
            }
            if self.worker.is_none() {
                if eos {
                    match self.held.back_mut() {
                        Some(last) if got => last.eos = true,
                        _ => self.eos_pending = true,
                    }
                }
                self.submits += 1;
                let n = self.submits;
                self.drain(true, fx);
                return Ok(n);
            }
        }
        // The worker decodes. The packet only waits in the queue.
        self.queued.push_back(Job {
            data: data.to_vec(),
            ts,
            eos,
        });
        self.cv.notify_all();
        self.submits += 1;
        let n = self.submits;
        self.drain(true, fx);
        Ok(n)
    }

    /// Starts the worker of the stream, which owns `pipe`. The decoder must be in a `Mutex` that `me` names.
    pub(super) fn start_worker(&mut self, pipe: Box<dyn Pipeline>) {
        let Some(dec) = self.me.upgrade() else {
            red("nn::codec: the decoder has no owner for a worker, it decodes in the call");
            return;
        };
        self.lag = pipe.lag();
        let spawned = std::thread::Builder::new()
            .name(format!("codec-{}", self.id))
            .stack_size(WORKER_STACK)
            .spawn(move || run_worker(&dec, pipe));
        match spawned {
            Ok(h) => {
                self.worker = Some(h);
                eprintln!(
                    "[codec] {} decoder {}: a worker thread decodes on the CPU, {} frame at a time",
                    self.codec.name(),
                    self.id,
                    self.lag
                );
            }
            Err(e) => {
                self.lag = 0;
                red(&format!("nn::codec: the worker did not start: {e}"));
            }
        }
    }

    /// Puts pictures and the end of stream into the released buffers, in order. The event says that a packet can
    /// come: it rises when a packet was taken (`took`) or when room came back, not at every release. A player that
    /// counts each signal as one packet it may send would overrun [`HOLD`] if it rose at every release.
    pub(super) fn drain(&mut self, took: bool, fx: &mut Effects) {
        let before = self.load();
        self.drain_from(before, took, fx);
    }

    /// [`Dec::drain`] for a caller that changed the load itself: `before` is the load before it did.
    pub(super) fn drain_from(&mut self, before: usize, took: bool, fx: &mut Effects) {
        if self.worker.is_some() && !self.pend.is_empty() && !self.held.is_empty() {
            // Pictures leave for buffers: the worker may decode ahead again.
            self.cv.notify_all();
        }
        while !self.pend.is_empty() && (!self.held.is_empty() || self.eos_pending) {
            let Some(i) = self.pend.pop_front() else {
                break;
            };
            let picture = self.held.pop_front();
            let Some(s) = self.slots.get_mut(i as usize) else {
                continue;
            };
            match picture {
                Some(h) => {
                    s.w = h.frame.width() as u16;
                    s.h = h.frame.height() as u16;
                    s.ts = h.ts;
                    s.eos = h.eos;
                    if let Some(pool) = &s.pool {
                        let mut p = lock(pool);
                        if s.pidx < p.count {
                            pool_native_write(&p, s.pidx, &h.frame);
                            if let Some(img) = p.imgs.get_mut(s.pidx as usize) {
                                *img = Some(h.frame);
                            }
                        }
                    }
                    if self.pictures == 0 {
                        eprintln!(
                            "[codec] {} decoder {}: first picture {}x{} into buffer {i}",
                            self.codec.name(),
                            self.id,
                            s.w,
                            s.h
                        );
                    }
                    self.pictures += 1;
                }
                None => {
                    s.w = 0;
                    s.h = 0;
                    s.ts = 0;
                    s.eos = true;
                    self.eos_pending = false;
                }
            }
            if s.eos {
                eprintln!(
                    "[codec] {} decoder {}: end of stream after {} pictures",
                    self.codec.name(),
                    self.id,
                    self.pictures
                );
            }
            s.filled = true;
            s.filled_generation = s.generation;
            fx.filled.push((i, s.generation));
        }
        if self.event != 0 {
            let limit = self.ready_limit();
            if self.load() >= limit {
                fx.event = Some(false);
            } else if took || before >= limit || self.event_state == Some(false) {
                // The last case: the event is down and there is room. Whoever lowered the load raises it, and so does
                // every later call, so it cannot stay down with room.
                fx.event = Some(true);
            }
        }
    }

    /// The oldest acquirable buffer and its sync handle.
    pub(crate) fn acquire(&mut self) -> Result<(u32, SyncHandle), u32> {
        let i = self.fifo.pop_front().ok_or(NONE);
        trace(format_args!(
            "dec {} acquire -> {i:?} (fifo {:?}, held {})",
            self.id,
            self.fifo,
            self.held.len()
        ));
        let i = i?;
        let generation = self.slots.get(i as usize).map_or(0, |s| s.generation);
        Ok((
            i,
            SyncHandle {
                magic: SYNC_MAGIC,
                dec: self.id,
                idx: i,
                generation,
            },
        ))
    }

    /// The buffer waits for the next picture and can be acquired again.
    pub(crate) fn release(&mut self, i: u32, fx: &mut Effects) {
        trace(format_args!(
            "dec {} release {i} (held {}, pend {:?})",
            self.id,
            self.held.len(),
            self.pend
        ));
        match self.slots.get_mut(i as usize) {
            Some(s) if s.reg => {
                s.generation = s.generation.wrapping_add(1);
                s.filled = false;
            }
            _ => {
                red(&format!(
                    "nn::codec: release of buffer {i}, which is not registered"
                ));
                return;
            }
        }
        if self.pend.len() < BUFS {
            self.pend.push_back(i);
        }
        if self.fifo.len() < FIFO {
            self.fifo.push_back(i);
        } else {
            red(&format!(
                "nn::codec: the queue of acquirable buffers is full at release {i}"
            ));
        }
        self.drain(false, fx);
    }

    /// The width, height, time and end of stream flag of the picture in buffer `i`. [`NONE`] until it is filled.
    pub(crate) fn properties(&self, i: u32) -> Result<(u16, u16, i64, bool), u32> {
        let s = self.slots.get(i as usize).ok_or(BAD)?;
        trace(format_args!(
            "dec {} properties {i} filled {}",
            self.id, s.filled
        ));
        if s.filled {
            Ok((s.w, s.h, s.ts, s.eos))
        } else {
            Err(NONE)
        }
    }

    pub(super) fn reached(&self, s: &SyncHandle) -> bool {
        self.slots
            .get(s.idx as usize)
            .is_some_and(|b| b.filled && b.filled_generation >= s.generation)
    }
}

/// Carries out `fx` for the decoder behind `d` and releases its lock. The order lock is taken before the decoder lock
/// is let go, so the effects of two calls happen in the order in which the calls decided them.
pub(super) fn deliver(mut d: MutexGuard<'_, Dec>, fx: Effects) {
    if let Some(up) = fx.event.filter(|&up| Some(up) != d.event_state) {
        trace(format_args!(
            "dec {} event {} (load {}, held {})",
            d.id,
            if up { "up" } else { "down" },
            d.load(),
            d.held.len()
        ));
    }
    d.event_state = fx.event.or(d.event_state);
    let (id, event, applier, order) = (d.id, d.event, Arc::clone(&d.applier), Arc::clone(&d.order));
    let turn = lock(&order);
    drop(d);
    applier(id, event, fx);
    drop(turn);
}

pub(super) fn has_effects(fx: &Effects) -> bool {
    fx.event.is_some() || !fx.filled.is_empty()
}
