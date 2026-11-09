//! The decode worker thread and the scheduling of events.

use super::*;

impl Shared {
    /// The event object for client event `i`. The client's struct is read again each time: a game registers it before
    /// it fills it, and the entry that was an event at registration is the fallback.
    pub(super) fn event(&self, st: &State, i: usize) -> Option<u64> {
        let live = if st.events_ptr == 0 {
            None
        } else {
            self.io.read_u64(st.events_ptr + 8 * i as u64)
        };
        match live {
            Some(p) if self.io.event_ok(p) => Some(p),
            _ => Some(st.events[i]).filter(|&p| p != 0),
        }
    }

    /// Signals client event `i` after `delay` on a guest thread, unless the decoder was stopped or reset by then.
    pub(super) fn schedule(self: &Arc<Shared>, st: &State, i: usize, delay: Duration) {
        let epoch = st.epoch;
        let me = Arc::clone(self);
        self.io.defer(
            delay,
            Box::new(move || {
                let st = lock(&me.state);
                if st.shutdown || !st.started || st.epoch != epoch {
                    return;
                }
                // The lock is held while the event is signalled, so a stop never interleaves with a signal.
                if let Some(p) = me.event(&st, i) {
                    me.io.signal(p);
                }
            }),
        );
    }

    /// Signals client event `i` now. The caller is on a guest thread and holds no lock.
    pub(super) fn signal_now(&self, i: usize) {
        let p = {
            let st = lock(&self.state);
            self.event(&st, i)
        };
        if let Some(p) = p {
            self.io.signal(p);
        }
    }
}

pub(crate) fn decode_packet(
    backend: &mut Option<(Box<dyn Backend>, u32, u32)>,
    open: Opener,
    data: &[u8],
) -> Result<Option<Arc<Frame>>, String> {
    if data.is_empty() {
        return Ok(None);
    }
    if let Some(k) = parse_key_frame(data) {
        let same = matches!(backend, Some((_, w, h)) if *w == k.width && *h == k.height);
        if !same {
            *backend = None;
            *backend = Some((open(&k)?, k.width, k.height));
        }
    }
    match backend {
        Some((b, _, _)) => b.decode(data),
        // A stream cannot start in the middle: packets before the first key frame show nothing.
        None => Ok(None),
    }
}

pub(super) fn worker(sh: &Arc<Shared>) {
    let mut backend: Option<(Box<dyn Backend>, u32, u32)> = None;
    let mut seen_close = 0;
    let mut st = lock(&sh.state);
    loop {
        if st.shutdown {
            return;
        }
        let Some(slot) = st.out.iter().position(|o| !o.used) else {
            st = sh.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let Some(packet) = st.pending.pop_front() else {
            st = sh.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        st.out[slot].used = true;
        let epoch = st.epoch;
        let close = st.close_gen;
        drop(st);

        if close != seen_close {
            backend = None;
            seen_close = close;
        }
        let result = decode_packet(&mut backend, sh.open, &packet.data);

        st = lock(&sh.state);
        if st.shutdown {
            return;
        }
        if st.epoch != epoch {
            // A reset put every slot back while this packet was decoding.
            continue;
        }
        st.inputs[packet.index] = InSlot::Free;
        let pic = match result {
            Ok(p) => p,
            Err(e) => {
                if st.logged & 2 == 0 {
                    st.logged |= 2;
                    eprintln!("[RED] video: a packet was not decoded: {e}");
                }
                None
            }
        };
        let has_pic = pic.is_some();
        if let Some(f) = pic {
            st.colorspace = f.colorspace();
            st.out[slot].pic = Some(f);
        }
        let ready = has_pic || packet.flags & FLAG_EOS != 0;
        if ready {
            st.out[slot].pts = packet.pts;
            st.out[slot].flags = packet.flags;
            st.ready.push_back(slot);
        } else {
            st.out[slot].used = false;
        }
        if !st.in_eos {
            sh.schedule(&st, 0, Duration::ZERO);
        }
        if ready {
            sh.schedule(&st, 1, Duration::ZERO);
        }
    }
}
