//! The tables of pools and decoders, the creation and finalisation of a decoder, and the sync event waits.

use super::*;

pub(super) static DECODERS: Mutex<BTreeMap<u32, Arc<Mutex<Dec>>>> = Mutex::new(BTreeMap::new());

/// The decoder whose handle the SDK object at `t` holds.
pub(crate) fn dec_of(t: u64) -> Option<Arc<Mutex<Dec>>> {
    let id = id_of(guest::read_u64(t).ok()?)?;
    lock(&DECODERS).get(&id).cloned()
}

/// The submission ready event of a decoder, a guest address.
pub(crate) fn dec_event(d: &Mutex<Dec>) -> u64 {
    lock(d).event()
}

/// Runs `f` on the decoder of the SDK object at `t` and then carries out what it asked for, with no lock held. `None`
/// when `t` holds no decoder.
pub(crate) fn dec_call<R>(t: u64, f: impl FnOnce(&mut Dec, &mut Effects) -> R) -> Option<R> {
    let dec = dec_of(t)?;
    let mut fx = Effects::default();
    let mut d = lock(&dec);
    let r = f(&mut d, &mut fx);
    deliver(d, fx);
    Some(r)
}

/// Stop and Flush: [`Dec::reset`], and then waits until the worker of the stream, if it has one, is idle with
/// everything dropped.
pub(crate) fn dec_reset(t: u64) -> Option<()> {
    let dec = dec_of(t)?;
    let mut fx = Effects::default();
    let mut d = lock(&dec);
    d.reset(&mut fx);
    let d = Dec::wait_idle(d);
    deliver(d, fx);
    Some(())
}

/// The picture in buffer `i` of the pool in the SDK object at `t`.
pub(crate) fn pool_frame(t: u64, i: u32) -> Option<Arc<Frame>> {
    let pool = pool_of(t)?;
    let p = lock(&pool);
    p.frame(i)
}

/// `Initialize…Decoder`: makes the decoder in the SDK object at `t`. VP9 and AV1 decode. The other codecs are refused
/// at once with a named line, so the game does not wait for pictures that never come.
pub(crate) fn create(t: u64, codec: Codec, w: u16, h: u16, input: u32, output: u32) -> u32 {
    let Some(cfg) = config() else {
        red("a decoder was made before the video service was initialised");
        return BAD;
    };
    if !matches!(codec, Codec::Vp9 | Codec::Av1) {
        red(&format!(
            "nn::codec {} decoders are not implemented, only VP9 and AV1. Initialize answers an error",
            codec.name()
        ));
        return BAD;
    }
    let event = t + EVENT_OFFSET;
    let dec = {
        let mut all = lock(&DECODERS);
        let Some(id) = (1..=MAX_DECODERS).find(|id| !all.contains_key(id)) else {
            red(&format!(
                "nn::codec: more than {MAX_DECODERS} video decoders"
            ));
            return BAD;
        };
        let dec = Arc::new(Mutex::new(Dec::new(id, codec, event)));
        lock(&dec).me = Arc::downgrade(&dec);
        all.insert(id, Arc::clone(&dec));
        dec
    };
    let id = lock(&dec).id();
    if guest::write_u64(t, handle_of(id)).is_err() {
        lock(&DECODERS).remove(&id);
        red(&format!(
            "the decoder object at {t:#x} is outside guest memory"
        ));
        return BAD;
    }
    cfg.host.bind_system_event(event);
    cfg.host.signal_system_event(event);
    eprintln!(
        "[codec] {} decoder {id}: {w}x{h}, input format {input}, output format {output} ({} on the host)",
        codec.name(),
        if codec == Codec::Av1 {
            "VideoToolbox or rav1d"
        } else {
            "VideoToolbox"
        }
    );
    0
}

/// `Finalize…Decoder`.
pub(crate) fn finalize(t: u64) -> u32 {
    let Some(id) = guest::read_u64(t).ok().and_then(id_of) else {
        return 0;
    };
    let Some(dec) = lock(&DECODERS).remove(&id) else {
        return 0;
    };
    // The worker first: it signals the event.
    stop_worker(&dec);
    let event = lock(&dec).event();
    // The event goes with the decoder, as in the SDK. Kept, it would stay bound, and the next decoder made in the same
    // memory would find its event bound and not get one.
    match crate::guest_api::os_fns() {
        Ok(f) => (f.destroy_system_event)(event),
        Err(e) => red(&format!(
            "the submission ready event of a decoder cannot be destroyed: {e}"
        )),
    }
    let _ = guest::write_u64(t, 0);
    0
}

/// Carries out what a call of a decoder asked for. The decoder lock is not held.
pub(crate) fn apply(dec: u32, event: u64, fx: Effects) {
    let Some(cfg) = config() else {
        return;
    };
    fire(&*cfg.host, dec, &fx.filled);
    match fx.event {
        Some(true) => cfg.host.signal_system_event(event),
        Some(false) => cfg.host.clear_system_event(event),
        None => {}
    }
}

/// Whether the picture that `s` waits for is in its buffer. A handle that the host did not make reads as signalled.
pub(crate) fn sync_reached(s: &SyncHandle) -> bool {
    let r = sync_reached_in(s);
    trace(format_args!(
        "sync dec {} idx {} gen {} reached {r}",
        s.dec, s.idx, s.generation
    ));
    r
}

pub(super) fn sync_reached_in(s: &SyncHandle) -> bool {
    if s.magic != SYNC_MAGIC {
        static LOGGED: AtomicU32 = AtomicU32::new(0);
        if (s.magic != 0 || s.dec != 0) && LOGGED.fetch_add(1, Ordering::Relaxed) == 0 {
            red(&format!(
                "nn::codec: a sync handle that the host did not make ({:#x}) reads as signalled",
                s.magic
            ));
        }
        return true;
    }
    if s.generation == 0 {
        return true;
    }
    let dec = lock(&DECODERS).get(&s.dec).cloned();
    dec.is_none_or(|d| lock(&d).reached(s))
}

// ---------- sync point events ----------

pub(super) struct Armed {
    event: u64,
    want: SyncHandle,
}

pub(super) static ARMED: Mutex<Vec<Armed>> = Mutex::new(Vec::new());

/// Arms `event` for `want`. False when too many events are armed.
pub(crate) fn arm(event: u64, want: SyncHandle) -> bool {
    let mut a = lock(&ARMED);
    if a.len() >= MAX_ARMED {
        return false;
    }
    a.push(Armed { event, want });
    true
}

pub(crate) fn unarm(event: u64) {
    lock(&ARMED).retain(|a| a.event != event);
}

/// Signals the armed events that wait for a buffer that was filled.
pub(super) fn fire(host: &dyn crate::Host, dec: u32, filled: &[(u32, u32)]) {
    if filled.is_empty() {
        return;
    }
    let mut due = Vec::new();
    lock(&ARMED).retain(|a| {
        let hit = a.want.dec == dec
            && filled
                .iter()
                .any(|&(i, g)| i == a.want.idx && g >= a.want.generation);
        if hit {
            due.push(a.event);
        }
        !hit
    });
    for e in due {
        host.signal_system_event(e);
    }
}
