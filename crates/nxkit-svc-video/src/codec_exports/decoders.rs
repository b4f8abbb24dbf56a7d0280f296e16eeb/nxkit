//! The decoder objects: initialisation, buffers, submission and output properties.

use super::*;

// ---------- decoders ----------

pub(super) extern "C" fn work_buffer(
    size: u64,
    align: u64,
    _bitstream: u64,
    _w: u64,
    _h: u64,
) -> u32 {
    bad_if(guest::write_u64(size, 0x1000).is_ok() && guest::write_u64(align, 0x1000).is_ok())
}

pub(super) fn init(t: u64, c: u8, w: u64, h: u64, input: u32, output: u32) -> u32 {
    create(t, codec_of(c), w as u16, h as u16, input, output)
}

pub(super) extern "C" fn init_full<const C: u8>(
    t: u64,
    _work: u64,
    _wsize: u64,
    _bs: u64,
    w: u64,
    h: u64,
    _refs: u32,
    input: u32,
    output: u64,
) -> u32 {
    init(t, C, w, h, input, output as u32)
}

pub(super) extern "C" fn init_remote<const C: u8>(
    t: u64,
    _bs: u64,
    w: u64,
    h: u64,
    _refs: u32,
    input: u32,
    output: u32,
) -> u32 {
    init(t, C, w, h, input, output)
}

#[allow(clippy::too_many_arguments)]
pub(super) extern "C" fn init_transfer<const C: u8>(
    t: u64,
    _work: u64,
    _wsize: u64,
    _bs: u64,
    w: u64,
    h: u64,
    _refs: u32,
    input: u32,
    output: u64,
    _mode: u64,
) -> u32 {
    init(t, C, w, h, input, output as u32)
}

#[allow(clippy::too_many_arguments)]
pub(super) extern "C" fn init_remote_mode<const C: u8>(
    t: u64,
    _bs: u64,
    w: u64,
    h: u64,
    _refs: u32,
    input: u32,
    output: u32,
    _mode: u32,
) -> u32 {
    init(t, C, w, h, input, output)
}

#[allow(clippy::too_many_arguments)]
pub(super) extern "C" fn init_remote19<const C: u8>(
    t: u64,
    _bs: u64,
    _mem: u64,
    w: u64,
    h: u64,
    _refs: u32,
    input: u32,
    output: u32,
) -> u32 {
    init(t, C, w, h, input, output)
}

pub(super) extern "C" fn finalize(t: u64) -> u32 {
    finalize_dec(t)
}

pub(super) extern "C" fn register(t: u64, i: u32, pool: u64, pidx: u32) -> u32 {
    dec_call(t, |d, _| d.register(i, pool_of(pool), pidx)).unwrap_or_else(|| {
        red(&format!("nn::codec::Register…DecoderBuffer index {i}"));
        BAD
    })
}

pub(super) extern "C" fn unregister(t: u64, i: u32) -> u32 {
    dec_call(t, |d, _| d.unregister(i)).unwrap_or(BAD)
}

pub(super) extern "C" fn start(t: u64) -> u32 {
    dec_call(t, |d, fx| {
        d.start(fx);
        0
    })
    .unwrap_or(BAD)
}

pub(super) extern "C" fn reset(t: u64) -> u32 {
    crate::codec::dec_reset(t).map_or(0, |()| 0)
}

pub(super) extern "C" fn ready_event_out(out: u64, t: u64) -> u32 {
    match dec_of(t) {
        Some(d) => {
            let event = crate::codec::dec_event(&d);
            bad_if(guest::write_u64(out, event).is_ok())
        }
        None => BAD,
    }
}

pub(super) extern "C" fn ready_event(t: u64) -> u64 {
    dec_of(t).map_or(0, |d| crate::codec::dec_event(&d))
}

pub(super) extern "C" fn submit(out: u64, t: u64, data: u64, size: u64, ts: u64, eos: u32) -> u32 {
    if dec_of(t).is_none() {
        return BAD;
    }
    if size > MAX_PACKET {
        red(&format!("nn::codec: a packet of {size} bytes is refused"));
        return BAD;
    }
    let mut packet = vec![0u8; size as usize];
    if size > 0 && guest::read(data, &mut packet).is_err() {
        red(&format!(
            "nn::codec: a packet at {data:#x} is outside guest memory"
        ));
        return BAD;
    }
    match dec_call(t, |d, fx| d.submit(&packet, ts as i64, eos & 0xff != 0, fx)) {
        Some(Ok(n)) => {
            let _ = guest::write_u64(out, n);
            0
        }
        Some(Err(e)) => e,
        None => BAD,
    }
}

pub(super) extern "C" fn submit_encrypted() -> u32 {
    static ONCE: AtomicBool = AtomicBool::new(false);
    red_once(
        &ONCE,
        "nn::codec: an encrypted input buffer: protected streams are not served",
    );
    BAD
}

pub(super) extern "C" fn acquire(idx: u64, sync: u64, t: u64) -> u32 {
    match dec_call(t, |d, _| d.acquire()) {
        Some(Ok((i, s))) => bad_if(guest::write_u32(idx, i).is_ok() && write_sync(sync, &s)),
        Some(Err(e)) => e,
        None => BAD,
    }
}

pub(super) extern "C" fn release(t: u64, i: u32, _sync_a: u64, _sync_b: u64) {
    if dec_call(t, |d, fx| d.release(i, fx)).is_none() {
        red(&format!("nn::codec::Release…DecoderOutputBuffer index {i}"));
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_properties(
    w: u64,
    h: u64,
    cw: u64,
    ch: u64,
    ts: u64,
    eos: u64,
    t: u64,
    i: u32,
) -> u32 {
    match dec_call(t, |d, _| d.properties(i)) {
        Some(Ok((pw, ph, pts, peos))) => bad_if(
            guest::write_u16(w, pw).is_ok()
                && guest::write_u16(h, ph).is_ok()
                && guest::write_u16(cw, pw).is_ok()
                && guest::write_u16(ch, ph).is_ok()
                && guest::write_u64(ts, pts as u64).is_ok()
                && guest::write_u8(eos, u8::from(peos)).is_ok(),
        ),
        Some(Err(e)) => e,
        None => BAD,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) extern "C" fn properties(
    w: u64,
    h: u64,
    cw: u64,
    ch: u64,
    ts: u64,
    eos: u64,
    t: u64,
    i: u32,
) -> u32 {
    write_properties(w, h, cw, ch, ts, eos, t, i)
}

/// The form of SDK 19 with a colour space that the game does not read, so it gets 0.
#[allow(clippy::too_many_arguments)]
pub(super) extern "C" fn properties19(
    w: u64,
    h: u64,
    cw: u64,
    ch: u64,
    cs: u64,
    ts: u64,
    eos: u64,
    t: u64,
    i: u64,
) -> u32 {
    let _ = guest::write_u32(cs, 0);
    write_properties(w, h, cw, ch, ts, eos, t, i as u32)
}
