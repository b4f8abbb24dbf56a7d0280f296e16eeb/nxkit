//! The functions that replace the `nn::codec` video decoders of the Switch 2 SDK (R43, R14).
//!
//! The decoders of every codec share one layout and one state ([`crate::codec`]). Only VP9 decodes. VP8, H.264 and AV1
//! answer Initialize with an error and a named line, so the game does not wait for pictures that never come.
//!
//! An argument of fewer than 32 bits comes as `u32` or `u64` and is masked where it is used, because Switch code does
//! not extend it. An argument past the eight registers is `u64`, because the guest gives each stack argument a slot of
//! eight bytes, and the host ABI of Apple packs them.

use std::sync::atomic::{AtomicBool, Ordering};

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use crate::codec::{
    BAD, Codec, POOL_ALIGN, Pref, SYNC_MAGIC, SyncHandle, arm, create, dec_call, dec_of,
    finalize as finalize_dec, native_handle, pool_bytes, pool_fin, pool_frame, pool_init, pool_of,
    sync_reached, unarm,
};
use crate::config;

const VP8: u8 = 0;
const VP9: u8 = 1;
const H264: u8 = 2;
const AV1: u8 = 3;

/// The largest packet that is copied out of the game.
const MAX_PACKET: u64 = 64 << 20;

fn codec_of(c: u8) -> Codec {
    match c {
        VP8 => Codec::Vp8,
        VP9 => Codec::Vp9,
        H264 => Codec::H264,
        _ => Codec::Av1,
    }
}

fn red(what: &str) {
    eprintln!("[RED] video: {what}");
}

fn red_once(flag: &AtomicBool, what: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        red(what);
    }
}

fn sync_from_regs(a: u64, b: u64) -> SyncHandle {
    SyncHandle {
        magic: a as u32,
        dec: (a >> 32) as u32,
        idx: b as u32,
        generation: (b >> 32) as u32,
    }
}

fn read_sync(addr: u64) -> Option<SyncHandle> {
    let a = guest::read_u64(addr).ok()?;
    let b = guest::read_u64(addr.checked_add(8)?).ok()?;
    Some(sync_from_regs(a, b))
}

fn write_sync(addr: u64, s: &SyncHandle) -> bool {
    let words = [s.magic, s.dec, s.idx, s.generation];
    let mut bytes = [0u8; 16];
    for (chunk, w) in bytes.chunks_exact_mut(4).zip(words) {
        chunk.copy_from_slice(&w.to_le_bytes());
    }
    guest::write(addr, &bytes).is_ok()
}

fn bad_if(ok: bool) -> u32 {
    if ok { 0 } else { BAD }
}

// ---------- the service ----------

extern "C" fn service_init() -> u32 {
    0
}

// ---------- image preferences and pools ----------

extern "C" fn pref(w: u64, h: u64, fmt: u32, mode: u32) -> Pref {
    Pref::new(u64::from(w as u16), u64::from(h as u16), fmt, mode)
}

extern "C" fn pool_size(size: u64, align: u64, pref_at: u64, n: u32) -> u32 {
    let Some(p) = Pref::read(pref_at) else {
        return BAD;
    };
    bad_if(
        guest::write_u64(size, pool_bytes(&p, n)).is_ok()
            && guest::write_u64(align, POOL_ALIGN).is_ok(),
    )
}

extern "C" fn pool_with_mode(t: u64, mem: u64, size: u64, pref_at: u64, n: u32, _mode: u32) -> u32 {
    pool_no_mode(t, mem, size, pref_at, n)
}

extern "C" fn pool_no_mode(t: u64, mem: u64, size: u64, pref_at: u64, n: u32) -> u32 {
    match Pref::read(pref_at) {
        Some(p) => pool_init(t, mem, size, &p, n),
        None => BAD,
    }
}

extern "C" fn pool_finalize(t: u64) {
    pool_fin(t);
}

extern "C" fn native_image_handle(t: u64, i: u32) -> u32 {
    pool_of(t).map_or(0, |p| native_handle(&p, i))
}

extern "C" fn read_rgba_hdr(
    _t: u64,
    _i: u32,
    _x: u64,
    _y: u64,
    _w: u64,
    _h: u64,
    dst: u64,
    _stride: u64,
    size: u64,
) -> u32 {
    static ONCE: AtomicBool = AtomicBool::new(false);
    red_once(
        &ONCE,
        "nn::codec::ReadDecoderOutputImageRgbaHdr: HDR pictures are not served, black",
    );
    let _ = guest::fill(dst, size, 0);
    0
}

extern "C" fn read_rgba(
    t: u64,
    i: u32,
    x: u64,
    y: u64,
    w: u64,
    h: u64,
    dst: u64,
    stride: u64,
    size: u64,
) -> u32 {
    static ONCE: AtomicBool = AtomicBool::new(false);
    let (x, y, w, h) = (x as u16, y as u16, u64::from(w as u16), u64::from(h as u16));
    let Some(frame) = pool_frame(t, i) else {
        let _ = guest::fill(dst, size, 0);
        return 0;
    };
    if x != 0 || y != 0 || stride != w * 4 || size < w * h * 4 {
        red_once(
            &ONCE,
            &format!(
                "nn::codec::ReadDecoderOutputImageRgba: region {x},{y} stride {stride}: only whole tight pictures are served"
            ),
        );
        return 0;
    }
    match frame.to_rgba(w as u32, h as u32) {
        Ok(rgba) => {
            let _ = guest::write(dst, &rgba);
        }
        Err(e) => red(&format!("nn::codec::ReadDecoderOutputImageRgba: {e}")),
    }
    0
}

/// The picture as a tight Y plane of `w * h` bytes and a tight UV plane of `w * h / 2` bytes. What the picture does not
/// cover is black.
fn tight_nv12(frame: &crate::Frame, w: usize, h: usize) -> Result<(Vec<u8>, Vec<u8>), String> {
    let planes = frame.to_nv12()?;
    let mut y = vec![16u8; w * h];
    let mut uv = vec![128u8; w * h / 2];
    let (fw, fh) = (frame.width() as usize, frame.height() as usize);
    for row in 0..h.min(fh) {
        let n = w.min(fw);
        if let (Some(src), Some(dst)) = (
            planes
                .y
                .get(row * planes.y_stride..row * planes.y_stride + n),
            y.get_mut(row * w..row * w + n),
        ) {
            dst.copy_from_slice(src);
        }
    }
    for row in 0..(h / 2).min(fh.div_ceil(2)) {
        let n = w.min(fw + (fw & 1));
        if let (Some(src), Some(dst)) = (
            planes
                .uv
                .get(row * planes.uv_stride..row * planes.uv_stride + n),
            uv.get_mut(row * w..row * w + n),
        ) {
            dst.copy_from_slice(src);
        }
    }
    Ok((y, uv))
}

#[allow(clippy::too_many_arguments)]
extern "C" fn read_nv12(
    t: u64,
    i: u32,
    x: u64,
    y: u64,
    w: u64,
    h: u64,
    ydst: u64,
    ystride: u64,
    ysize: u64,
    uvdst: u64,
    uvstride: u64,
    uvsize: u64,
) -> u32 {
    static ONCE: AtomicBool = AtomicBool::new(false);
    let (x, y, w, h) = (x as u16, y as u16, u64::from(w as u16), u64::from(h as u16));
    if x != 0 || y != 0 || ystride != w || uvstride != w || ysize < w * h || uvsize < w * h / 2 {
        red_once(
            &ONCE,
            &format!(
                "nn::codec::ReadDecoderOutputImageNv12: region {x},{y} strides {ystride} / {uvstride}: only whole tight pictures are served"
            ),
        );
        return 0;
    }
    let Some(frame) = pool_frame(t, i) else {
        let _ = guest::fill(ydst, ysize, 16);
        let _ = guest::fill(uvdst, uvsize, 128);
        return 0;
    };
    match tight_nv12(&frame, w as usize, h as usize) {
        Ok((yp, uvp)) => {
            let _ = guest::write(ydst, &yp);
            let _ = guest::write(uvdst, &uvp);
        }
        Err(e) => red(&format!("nn::codec::ReadDecoderOutputImageNv12: {e}")),
    }
    0
}

// ---------- decoders ----------

extern "C" fn work_buffer(size: u64, align: u64, _bitstream: u64, _w: u64, _h: u64) -> u32 {
    bad_if(guest::write_u64(size, 0x1000).is_ok() && guest::write_u64(align, 0x1000).is_ok())
}

fn init(t: u64, c: u8, w: u64, h: u64, input: u32, output: u32) -> u32 {
    create(t, codec_of(c), w as u16, h as u16, input, output)
}

extern "C" fn init_full<const C: u8>(
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

extern "C" fn init_remote<const C: u8>(
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
extern "C" fn init_transfer<const C: u8>(
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
extern "C" fn init_remote_mode<const C: u8>(
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
extern "C" fn init_remote19<const C: u8>(
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

extern "C" fn finalize(t: u64) -> u32 {
    finalize_dec(t)
}

extern "C" fn register(t: u64, i: u32, pool: u64, pidx: u32) -> u32 {
    dec_call(t, |d, _| d.register(i, pool_of(pool), pidx)).unwrap_or_else(|| {
        red(&format!("nn::codec::Register…DecoderBuffer index {i}"));
        BAD
    })
}

extern "C" fn unregister(t: u64, i: u32) -> u32 {
    dec_call(t, |d, _| d.unregister(i)).unwrap_or(BAD)
}

extern "C" fn start(t: u64) -> u32 {
    dec_call(t, |d, fx| {
        d.start(fx);
        0
    })
    .unwrap_or(BAD)
}

extern "C" fn reset(t: u64) -> u32 {
    dec_call(t, |d, fx| {
        d.reset(fx);
        0
    })
    .unwrap_or(0)
}

extern "C" fn ready_event_out(out: u64, t: u64) -> u32 {
    match dec_of(t) {
        Some(d) => {
            let event = crate::codec::dec_event(&d);
            bad_if(guest::write_u64(out, event).is_ok())
        }
        None => BAD,
    }
}

extern "C" fn ready_event(t: u64) -> u64 {
    dec_of(t).map_or(0, |d| crate::codec::dec_event(&d))
}

extern "C" fn submit(out: u64, t: u64, data: u64, size: u64, ts: u64, eos: u32) -> u32 {
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

extern "C" fn submit_encrypted() -> u32 {
    static ONCE: AtomicBool = AtomicBool::new(false);
    red_once(
        &ONCE,
        "nn::codec: an encrypted input buffer: protected streams are not served",
    );
    BAD
}

extern "C" fn acquire(idx: u64, sync: u64, t: u64) -> u32 {
    match dec_call(t, |d, _| d.acquire()) {
        Some(Ok((i, s))) => bad_if(guest::write_u32(idx, i).is_ok() && write_sync(sync, &s)),
        Some(Err(e)) => e,
        None => BAD,
    }
}

extern "C" fn release(t: u64, i: u32, _sync_a: u64, _sync_b: u64) {
    if dec_call(t, |d, fx| d.release(i, fx)).is_none() {
        red(&format!("nn::codec::Release…DecoderOutputBuffer index {i}"));
    }
}

#[allow(clippy::too_many_arguments)]
fn write_properties(w: u64, h: u64, cw: u64, ch: u64, ts: u64, eos: u64, t: u64, i: u32) -> u32 {
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
extern "C" fn properties(
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
extern "C" fn properties19(
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

// ---------- sync handles and sync point events ----------

extern "C" fn try_wait_native_sync(sync: u64) -> u32 {
    read_sync(sync).is_none_or(|s| sync_reached(&s)).into()
}

fn event_signal(e: u64) {
    if let Some(c) = config() {
        c.host.signal_system_event(e);
    }
}

fn event_clear(e: u64) {
    if let Some(c) = config() {
        c.host.clear_system_event(e);
    }
}

/// Where the awaited handle lies inside a sync point event of the SDK. The magic that marks the event as the host's
/// is the first word.
const SPT_MAGIC_AT: u64 = 0x40;
/// The bytes of a sync point event that the host owns: a system event of 0x30 bytes, and the awaited handle.
const SPT_BYTES: u64 = 0x58;

extern "C" fn spt_create(e: u64) -> u32 {
    let Some(c) = config() else {
        return BAD;
    };
    if guest::fill(e, SPT_BYTES, 0).is_err() {
        return BAD;
    }
    c.host.bind_system_event(e);
    let _ = guest::write_u32(e + SPT_MAGIC_AT, SYNC_MAGIC);
    0
}

extern "C" fn spt_holder(holder: u64, e: u64) {
    match crate::guest_api::os_fns() {
        Ok(f) => (f.init_multi_wait_holder)(holder, e),
        Err(err) => red(&format!(
            "nn::syncpt::sdk::InitializeMultiWaitHolder: {err}"
        )),
    }
}

extern "C" fn spt_request(e: u64, sync: u64) -> u32 {
    unarm(e);
    event_clear(e);
    let Some(want) = read_sync(sync) else {
        return BAD;
    };
    if !arm(e, want) {
        red("nn::syncpt: more than 64 armed events: signalled at once");
        event_signal(e);
        return 0;
    }
    if sync_reached(&want) {
        unarm(e);
        event_signal(e);
    }
    0
}

extern "C" fn spt_cancel(e: u64) {
    unarm(e);
    event_clear(e);
}

extern "C" fn spt_try_wait(e: u64) -> u32 {
    match crate::guest_api::os_fns() {
        Ok(f) => u32::from((f.try_wait_system_event)(e) & 0xff != 0),
        Err(err) => {
            red(&format!("nn::syncpt::sdk::TryWaitSyncPointEvent: {err}"));
            0
        }
    }
}

extern "C" fn spt_destroy(e: u64) {
    unarm(e);
    match crate::guest_api::os_fns() {
        Ok(f) => (f.destroy_system_event)(e),
        Err(err) => red(&format!("nn::syncpt::sdk::DestroySyncPointEvent: {err}")),
    }
    let _ = guest::write_u32(e + SPT_MAGIC_AT, 0);
}

// ---------- the table ----------

/// The mangled name of a function of `nn::codec`: `f` is the function name and `rest` the encoded arguments.
fn method(f: &str, rest: &str) -> String {
    format!("_ZN2nn5codec{}{f}{rest}", f.len())
}

fn push(all: &mut Vec<Export>, name: String, addr: usize) {
    all.push(Export::new(Box::leak(name.into_boxed_str()), addr));
}

const INPUT: &str = "NS0_23VideoDecoderInputFormatE";
const OUTPUT: &str = "NS0_24VideoDecoderOutputFormatE";
const MODE: &str = "NS0_22VideoDecoderOutputModeE";

/// The entry points of the decoder type `tag` (`Vp9`, `H264`, ...) of codec `C`.
fn family<const C: u8>(tag: &str, all: &mut Vec<Export>) {
    let ty = format!("{}{tag}DecoderType", tag.len() + 11);
    let s = format!("PNS0_{ty}E");
    let csd = if C == H264 { "PKvm" } else { "" };
    let mut add = |f: String, rest: String, addr: usize| push(all, method(&f, &rest), addr);
    add(
        format!("Calculate{tag}DecoderWorkBuffer"),
        "EPmS1_mtt".into(),
        work_buffer as usize,
    );
    add(
        format!("Initialize{tag}Decoder"),
        format!("E{s}Pvmmttj{INPUT}{OUTPUT}{csd}"),
        init_full::<C> as usize,
    );
    add(
        format!("Initialize{tag}Decoder"),
        format!("E{s}mttj{INPUT}{OUTPUT}{csd}"),
        init_remote::<C> as usize,
    );
    add(
        format!("Finalize{tag}Decoder"),
        format!("E{s}"),
        finalize as usize,
    );
    add(
        format!("Register{tag}DecoderBuffer"),
        format!("E{s}jPNS0_25VideoDecoderImagePoolTypeEj"),
        register as usize,
    );
    add(
        format!("Unregister{tag}DecoderBuffer"),
        format!("E{s}j"),
        unregister as usize,
    );
    add(
        format!("Start{tag}Decoder"),
        format!("E{s}"),
        start as usize,
    );
    add(format!("Stop{tag}Decoder"), format!("E{s}"), reset as usize);
    add(
        format!("Flush{tag}Decoder"),
        format!("E{s}"),
        reset as usize,
    );
    add(
        format!("Get{tag}DecoderSubmissionReadyEvent"),
        format!("EPPNS_2os15SystemEventTypeE{s}"),
        ready_event_out as usize,
    );
    add(
        format!("Submit{tag}DecoderInputBuffer"),
        format!("EPm{s}PKvmNS_8TimeSpanEb"),
        submit as usize,
    );
    add(
        format!("SubmitEncrypted{tag}DecoderInputBuffer"),
        format!("EPm{s}PKNS0_18EncryptionPropertyEPKvmNS_8TimeSpanEb"),
        submit_encrypted as usize,
    );
    add(
        format!("Acquire{tag}DecoderOutputBuffer"),
        format!("EPjPNS_2gs16NativeSyncHandleE{s}"),
        acquire as usize,
    );
    add(
        format!("Release{tag}DecoderOutputBuffer"),
        format!("E{s}jNS_2gs16NativeSyncHandleE"),
        release as usize,
    );
    add(
        format!("Get{tag}DecoderOutputBufferProperties"),
        format!("EPtS1_S1_S1_PNS_8TimeSpanEPb{s}j"),
        properties as usize,
    );
    // The forms of SDK 22.
    add(
        format!("Initialize{tag}DecoderByTransferMemory"),
        format!("E{s}Pvmmttj{INPUT}{OUTPUT}{MODE}{csd}"),
        init_transfer::<C> as usize,
    );
    add(
        format!("Initialize{tag}DecoderByRemoteMemory"),
        format!("E{s}mttj{INPUT}{OUTPUT}{MODE}{csd}"),
        init_remote_mode::<C> as usize,
    );
    add(
        format!("Get{tag}DecoderSubmissionReadyEvent"),
        format!("E{s}"),
        ready_event as usize,
    );
    // The forms of SDK 19.
    add(
        format!("Initialize{tag}Decoder"),
        format!("E{s}mmttj{INPUT}{OUTPUT}{csd}"),
        init_remote19::<C> as usize,
    );
    add(
        format!("Get{tag}DecoderOutputBufferProperties"),
        format!("EPtS1_S1_S1_PNS_5movie10ColorSpaceEPNS_8TimeSpanEPb{s}j"),
        properties19 as usize,
    );
    if C == H264 {
        add(
            format!("Initialize{tag}Decoder"),
            format!("E{s}Pvmmttj{INPUT}{OUTPUT}{MODE}{csd}"),
            init_transfer::<C> as usize,
        );
    }
}

/// The functions that this module gives the game.
pub(crate) fn table() -> Vec<Export> {
    let mut all = Vec::new();
    let pool_type = "NS0_25VideoDecoderImagePoolTypeE";
    let pref_ref = "RKNS0_27VideoDecoderImagePreferenceE";
    let mut add = |f: &str, rest: String, addr: usize| push(&mut all, method(f, &rest), addr);
    add(
        "MakeDecodeImagePreference",
        "EttNS0_24VideoDecoderOutputFormatENS0_9CodecModeE".to_string(),
        pref as usize,
    );
    add(
        "MakeDecodeImagePreference",
        "EttNS0_24VideoDecoderOutputFormatENS0_15ImageTilingModeE".to_string(),
        pref as usize,
    );
    add(
        "CalculateDecoderImagePoolBuffer",
        format!("EPmS1_{pref_ref}j"),
        pool_size as usize,
    );
    add(
        "InitializeDecoderImagePool",
        format!("EP{pool_type}Pvm{pref_ref}jNS0_9CodecModeE"),
        pool_with_mode as usize,
    );
    add(
        "InitializeDecoderImagePool",
        format!("EP{pool_type}Pvm{pref_ref}j"),
        pool_no_mode as usize,
    );
    add(
        "FinalizeDecoderImagePool",
        format!("EP{pool_type}"),
        pool_finalize as usize,
    );
    add(
        "GetDecoderImagePoolNativeImageHandle",
        format!("EPK{pool_type}j"),
        native_image_handle as usize,
    );
    add(
        "ReadDecoderOutputImageRgbaHdr",
        format!("EPK{pool_type}jttttPvmm"),
        read_rgba_hdr as usize,
    );
    add(
        "ReadDecoderOutputImageRgba",
        format!("EPK{pool_type}jttttPvmm"),
        read_rgba as usize,
    );
    add(
        "ReadDecoderOutputImageNv12",
        format!("EPK{pool_type}jttttPvmmS4_mm"),
        read_nv12 as usize,
    );
    push(
        &mut all,
        "_ZN2nn5codec3sdk34MakeDecodeImagePreferenceForCompatEttNS0_24VideoDecoderOutputFormatENS0_9CodecModeE".to_string(),
        pref as usize,
    );
    push(
        &mut all,
        "_ZN2nn5codec5video10InitializeEv".to_string(),
        service_init as usize,
    );
    push(
        &mut all,
        "_ZN2nn5codec5video8FinalizeEv".to_string(),
        service_init as usize,
    );
    family::<VP8>("Vp8", &mut all);
    family::<VP9>("Vp9", &mut all);
    family::<H264>("H264", &mut all);
    family::<AV1>("Av1", &mut all);
    let fixed: [(&str, usize); 7] = [
        (
            "_ZN2nn2gs17TryWaitNativeSyncERKNS0_16NativeSyncHandleE",
            try_wait_native_sync as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk20CreateSyncPointEventEPNS1_18SyncPointEventTypeE",
            spt_create as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk25InitializeMultiWaitHolderEPNS_2os19MultiWaitHolderTypeEPNS1_18SyncPointEventTypeE",
            spt_holder as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk21RequestSyncPointEventEPNS1_18SyncPointEventTypeERKNS1_15SyncPointHandleE",
            spt_request as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk20CancelSyncPointEventEPNS1_18SyncPointEventTypeE",
            spt_cancel as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk21TryWaitSyncPointEventEPKNS1_18SyncPointEventTypeE",
            spt_try_wait as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk21DestroySyncPointEventEPNS1_18SyncPointEventTypeE",
            spt_destroy as usize,
        ),
    ];
    for (name, addr) in fixed {
        all.push(Export::new(name, addr));
    }
    all
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn names_follow_the_itanium_scheme_of_the_sdk() {
        let all = table();
        let has = |n: &str| all.iter().any(|e| e.name == n);
        assert!(has("_ZN2nn5codec29CalculateVp9DecoderWorkBufferEPmS1_mtt"));
        assert!(has(
            "_ZN2nn5codec31CalculateDecoderImagePoolBufferEPmS1_RKNS0_27VideoDecoderImagePreferenceEj"
        ));
        assert!(has(
            "_ZN2nn5codec26InitializeDecoderImagePoolEPNS0_25VideoDecoderImagePoolTypeEPvmRKNS0_27VideoDecoderImagePreferenceEjNS0_9CodecModeE"
        ));
        assert!(has(
            "_ZN2nn5codec26ReadDecoderOutputImageRgbaEPKNS0_25VideoDecoderImagePoolTypeEjttttPvmm"
        ));
        assert!(has(
            "_ZN2nn5codec36GetH264DecoderOutputBufferPropertiesEPtS1_S1_S1_PNS_8TimeSpanEPbPNS0_15H264DecoderTypeEj"
        ));
        assert!(has(
            "_ZN2nn5codec21InitializeH264DecoderEPNS0_15H264DecoderTypeEPvmmttjNS0_23VideoDecoderInputFormatENS0_24VideoDecoderOutputFormatENS0_22VideoDecoderOutputModeEPKvm"
        ));
    }

    #[test]
    fn the_codec_video_names_of_the_media_group_are_here() {
        let all = table();
        let names = [
            "_ZN2nn5codec25MakeDecodeImagePreferenceEttNS0_24VideoDecoderOutputFormatENS0_9CodecModeE",
            "_ZN2nn5codec25MakeDecodeImagePreferenceEttNS0_24VideoDecoderOutputFormatENS0_15ImageTilingModeE",
            "_ZN2nn5codec24FinalizeDecoderImagePoolEPNS0_25VideoDecoderImagePoolTypeE",
            "_ZN2nn5codec36GetDecoderImagePoolNativeImageHandleEPKNS0_25VideoDecoderImagePoolTypeEj",
            "_ZN2nn5codec29ReadDecoderOutputImageRgbaHdrEPKNS0_25VideoDecoderImagePoolTypeEjttttPvmm",
            "_ZN2nn5codec26ReadDecoderOutputImageNv12EPKNS0_25VideoDecoderImagePoolTypeEjttttPvmmS4_mm",
            "_ZN2nn5codec37InitializeH264DecoderByTransferMemoryEPNS0_15H264DecoderTypeEPvmmttjNS0_23VideoDecoderInputFormatENS0_24VideoDecoderOutputFormatENS0_22VideoDecoderOutputModeEPKvm",
            "_ZN2nn5codec35InitializeH264DecoderByRemoteMemoryEPNS0_15H264DecoderTypeEmttjNS0_23VideoDecoderInputFormatENS0_24VideoDecoderOutputFormatENS0_22VideoDecoderOutputModeEPKvm",
            "_ZN2nn5codec34GetH264DecoderSubmissionReadyEventEPNS0_15H264DecoderTypeE",
            "_ZN2nn5codec21InitializeH264DecoderEPNS0_15H264DecoderTypeEmmttjNS0_23VideoDecoderInputFormatENS0_24VideoDecoderOutputFormatEPKvm",
            "_ZN2nn5codec36GetH264DecoderOutputBufferPropertiesEPtS1_S1_S1_PNS_5movie10ColorSpaceEPNS_8TimeSpanEPbPNS0_15H264DecoderTypeEj",
        ];
        for n in names {
            assert!(all.iter().any(|e| e.name == n), "{n}");
        }
    }

    #[test]
    fn no_name_is_given_twice() {
        let all = table();
        let names: BTreeSet<&str> = all.iter().map(|e| e.name).collect();
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn every_name_of_the_nxnative_take_over_is_here() {
        // 4 decoder types of 20 entries, H.264 one more, 10 pool and preference ones, the compat preference,
        // the 2 service ones and 7 sync ones.
        assert_eq!(table().len(), 4 * 20 + 1 + 10 + 1 + 2 + 7);
    }
}
