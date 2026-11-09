//! The worker thread of a stream that decodes on the CPU.

use super::*;

/// The call of the decoder that the worker made.
#[derive(Clone, Copy)]
pub(super) enum Call {
    /// A packet. `taken` says whether the decoder took it in: if not, it is not in flight.
    Job {
        eos: bool,
        taken: bool,
    },
    Wait,
}

/// What the worker found after a call of the decoder, under the lock.
pub(super) fn absorb(
    d: &mut Dec,
    call: Call,
    res: Result<(), String>,
    out: &mut Vec<Pic>,
) -> Effects {
    let before = d.load();
    let (job_eos, popped) = match call {
        Call::Job { eos, taken } => (eos, !taken || res.is_err()),
        Call::Wait => (false, false),
    };
    let mut fx = Effects::default();
    if d.flush_req {
        // A Stop or Flush came meanwhile: what the decoder gave belongs to the stream that ended.
        out.clear();
        return fx;
    }
    if popped {
        // The packet was dropped or failed: no picture follows.
        d.inflight = d.inflight.saturating_sub(1);
    }
    if let Err(e) = &res
        && d.errors < 4
    {
        d.errors += 1;
        warn(&format!("nn::codec {}: {e}", d.codec.name()));
    }
    d.inflight = d.inflight.saturating_sub(out.len());
    if matches!(call, Call::Wait) && res.is_ok() && out.is_empty() {
        // Nothing was in flight: a packet that shows no picture (a sequence header alone) was counted.
        d.inflight = 0;
    }
    if job_eos {
        // The decoder was drained: nothing is in flight, and the last picture ends the stream.
        d.inflight = 0;
        if out.is_empty() {
            d.eos_pending = true;
        } else {
            let last = out.len() - 1;
            for (k, (frame, ts)) in out.drain(..).enumerate() {
                if d.hold(frame, ts)
                    && k == last
                    && let Some(h) = d.held.back_mut()
                {
                    h.eos = true;
                }
            }
        }
    } else {
        for (frame, ts) in out.drain(..) {
            d.hold(frame, ts);
        }
    }
    d.drain_from(before, false, &mut fx);
    fx
}

/// The worker of a stream: sends the packets of the queue, and waits for pictures whenever frames are in flight and
/// no packet is queued.
pub(super) fn run_worker(dec: &Arc<Mutex<Dec>>, mut pipe: Box<dyn Pipeline>) {
    let cv = Arc::clone(&lock(dec).cv);
    let mut out: Vec<Pic> = Vec::new();
    // Pictures may still be inside the decoder, whatever the count says.
    let mut maybe = false;
    let mut d = lock(dec);
    loop {
        if d.stop {
            return;
        }
        if d.flush_req {
            pipe.flush();
            maybe = false;
            d.flush_req = false;
            d.inflight = 0;
            cv.notify_all();
            continue;
        }
        if d.held.len() >= super::LOOKAHEAD {
            // Enough pictures wait for the game. A release wakes the worker (`drain`).
            let (g, timeout) = cv
                .wait_timeout(d, Duration::from_secs(1))
                .unwrap_or_else(PoisonError::into_inner);
            d = g;
            if timeout.timed_out() && !d.stop {
                d.diag_tick("worker");
            }
            continue;
        }
        if let Some(job) = d.queued.pop_front() {
            d.inflight += 1;
            d.busy = true;
            drop(d);
            out.clear();
            let mut res = Ok(());
            let mut taken = false;
            if !job.data.is_empty() {
                match pipe.push(&job.data, job.ts, &mut out) {
                    Ok(t) => taken = t,
                    Err(e) => res = Err(e),
                }
            }
            maybe |= taken || !out.is_empty();
            if job.eos && res.is_ok() {
                res = pipe.finish(&mut out);
                maybe = false;
            }
            d = lock(dec);
            d.busy = false;
            let fx = absorb(
                &mut d,
                Call::Job {
                    eos: job.eos,
                    taken,
                },
                res,
                &mut out,
            );
            cv.notify_all();
            if has_effects(&fx) {
                deliver(d, fx);
                d = lock(dec);
            }
        } else if d.inflight > 0 || maybe {
            // Pictures may be inside: a unit can show more or fewer than one, so the count alone does not say.
            d.busy = true;
            drop(d);
            out.clear();
            let res = pipe.wait(&mut out);
            if res.is_ok() && out.is_empty() {
                maybe = false;
            }
            d = lock(dec);
            d.busy = false;
            let fx = absorb(&mut d, Call::Wait, res, &mut out);
            cv.notify_all();
            if has_effects(&fx) {
                deliver(d, fx);
                d = lock(dec);
            }
        } else {
            // Idle. Every second the state line, and the event is checked: it must be up when there is room.
            let (g, timeout) = cv
                .wait_timeout(d, Duration::from_secs(1))
                .unwrap_or_else(PoisonError::into_inner);
            d = g;
            if timeout.timed_out() && !d.stop {
                d.diag_tick("worker");
                let mut fx = Effects::default();
                let load = d.load();
                d.drain_from(load, false, &mut fx);
                if has_effects(&fx) {
                    deliver(d, fx);
                    d = lock(dec);
                }
            }
        }
    }
}

/// Stops the worker of the decoder and joins it. The stream is dropped with it.
pub(crate) fn stop_worker(dec: &Mutex<Dec>) {
    let handle = {
        let mut d = lock(dec);
        d.stop = true;
        d.cv.notify_all();
        d.worker.take()
    };
    if let Some(h) = handle
        && h.thread().id() != std::thread::current().id()
    {
        let _ = h.join();
    }
}
