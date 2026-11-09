//! The functions that replace the methods of `movie::Decoder` in the game's SDK (R14).
//!
//! A decoder object of the game is two words. The first is the vtable of the SDK class, and the second is a handle of
//! the decoder in this crate. The game's own constructors and destructors are replaced, so the game never runs code of
//! the SDK decoder, which would open the NVIDIA decoder device that the host does not have (R43).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use nxkit_abi::export::Export;
use nxkit_abi::guest;

use crate::decoder::{
    Decoder, ERR_BAD_ARG, ERR_NO_IMPL, ERR_START, ERR_STOP, IN_CAP, Io, OUT_COUNT,
};
use crate::guest_api::{Fns, fns};
use crate::{Host, config, pacing};

const TYPE_VP9: u32 = 3;
const FORMAT_ABGR: u32 = 3;
const FORMAT_NV12: u32 = 4;
const MODE_CPU: u32 = 1;
const MODE_TEXTURE: u32 = 2;

/// A decoder of the game with the settings of its constructor.
struct Object {
    decoder: Decoder,
    mode: u32,
    /// SDK 3 reports an alignment where later SDKs report the mode. Zero means the mode.
    out_align: u32,
}

static REGISTRY: Mutex<BTreeMap<u64, Arc<Object>>> = Mutex::new(BTreeMap::new());
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn registry() -> MutexGuard<'static, BTreeMap<u64, Arc<Object>>> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How the decoder reaches the host and the game.
struct GuestIo {
    host: Arc<dyn Host>,
}

impl Io for GuestIo {
    fn defer(&self, delay: Duration, job: Box<dyn FnOnce() + Send>) {
        self.host.run_on_guest_thread(delay, job);
    }

    fn signal(&self, event: u64) {
        if let Ok(f) = fns() {
            (f.signal_event)(event);
        }
    }

    fn event_ok(&self, addr: u64) -> bool {
        addr >= 0x1_0000_0000 && addr.is_multiple_of(8) && guest::check(addr, 8).is_ok()
    }

    fn read_u64(&self, addr: u64) -> Option<u64> {
        guest::read_u64(addr).ok()
    }
}

fn object(this: u64) -> Option<Arc<Object>> {
    let handle = guest::read_u64(this.checked_add(8)?).ok()?;
    if handle == 0 {
        return None;
    }
    registry().get(&handle).cloned()
}

fn red(what: &str) {
    eprintln!("[RED] video: {what}");
}

fn construct(this: u64, kind: u32, format: u32, mode: u32, out_align: u32) {
    let f = match fns() {
        Ok(f) => f,
        Err(e) => {
            red(&format!("a movie decoder cannot be made: {e}"));
            return;
        }
    };
    if guest::write_u64(this, f.vtable).is_err() || guest::write_u64(this + 8, 0).is_err() {
        red(&format!(
            "the decoder object at {this:#x} is outside guest memory"
        ));
        return;
    }
    let Some(cfg) = config() else {
        red("a movie decoder was made before the video service was initialised");
        return;
    };
    if kind != TYPE_VP9 {
        red(&format!(
            "decoder type {kind} is not implemented, only VP9 (3). Every call on it answers {ERR_NO_IMPL:#x}"
        ));
        return;
    }
    let format_ok = format == FORMAT_NV12 || (format == FORMAT_ABGR && mode == MODE_TEXTURE);
    if !format_ok || (mode != MODE_CPU && mode != MODE_TEXTURE) {
        red(&format!(
            "output format {format} in mode {mode} is not implemented, only NV12 and ABGR in native texture mode"
        ));
        return;
    }
    if mode == MODE_TEXTURE && cfg.sink.is_none() {
        red(
            "the game asks for decoder output in native textures and the host gave no texture sink",
        );
        return;
    }
    let io = Arc::new(GuestIo {
        host: Arc::clone(&cfg.host),
    });
    let decoder = match Decoder::new(io, crate::backend::open) {
        Ok(d) => d,
        Err(e) => {
            red(&e);
            return;
        }
    };
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    registry().insert(
        handle,
        Arc::new(Object {
            decoder,
            mode,
            out_align,
        }),
    );
    if guest::write_u64(this + 8, handle).is_err() {
        registry().remove(&handle);
    }
}

extern "C" fn ctor_full(this: u64, kind: u32, format: u32, mode: u32, _priority: i32) {
    construct(this, kind, format, mode, 0);
}

extern "C" fn ctor_no_priority(this: u64, kind: u32, format: u32, mode: u32) {
    construct(this, kind, format, mode, 0);
}

/// SDK 3 has no mode. Its decoders are CPU decoders, and its format numbers are shifted against later SDKs.
extern "C" fn ctor_sdk3(this: u64, kind: u32, format: u32) {
    let format = match format {
        3 => FORMAT_NV12,
        2 => FORMAT_ABGR,
        other => other,
    };
    construct(this, kind, format, MODE_CPU, 0x20);
}

fn destroy(this: u64) {
    let Ok(handle) = guest::read_u64(this + 8) else {
        return;
    };
    if handle == 0 {
        return;
    }
    let object = registry().remove(&handle);
    let _ = guest::write_u64(this + 8, 0);
    // The decoder thread is joined here, outside the registry lock.
    drop(object);
}

extern "C" fn dtor(this: u64) {
    destroy(this);
}

extern "C" fn dtor_delete(this: u64) {
    destroy(this);
    match fns() {
        Ok(f) => (f.delete)(this),
        Err(e) => red(&format!("a movie decoder cannot be deleted: {e}")),
    }
}

extern "C" fn configure(this: u64, media_data: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    let (mut w, mut h) = (0i32, 0i32);
    if media_data != 0
        && let Ok(f) = fns()
    {
        (f.find_int32)(media_data, c"width".as_ptr(), &raw mut w);
        (f.find_int32)(media_data, c"height".as_ptr(), &raw mut h);
    }
    o.decoder.configure(w, h);
    0
}

extern "C" fn register_client_events(this: u64, events: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    o.decoder.register_events(events);
    0
}

extern "C" fn start(this: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_START;
    };
    o.decoder.start();
    0
}

extern "C" fn stop(this: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_STOP;
    };
    o.decoder.stop();
    0
}

extern "C" fn reset(this: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    o.decoder.reset();
    0
}

extern "C" fn flush(this: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    o.decoder.flush();
    0
}

extern "C" fn acquire_input_buffer_index(this: u64, index: u64, left: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    if index == 0 || left == 0 {
        return ERR_BAD_ARG;
    }
    let (i, n, code) = match o.decoder.acquire_input() {
        Ok((i, n)) => (i, n, 0),
        Err(code) => (-1, 0, code),
    };
    if guest::write_i32(index, i).is_err() || guest::write_i32(left, n).is_err() {
        return ERR_BAD_ARG;
    }
    code
}

extern "C" fn get_input_buffer(this: u64, index: i32, buffer: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    let (Some(addr), true) = (o.decoder.input_addr(index), buffer != 0) else {
        return ERR_BAD_ARG;
    };
    let f = match fns() {
        Ok(f) => f,
        Err(e) => {
            red(&e);
            return ERR_NO_IMPL;
        }
    };
    (f.set_data)(buffer, addr, IN_CAP as u64);
    (f.set_range)(buffer, 0, IN_CAP as i32);
    0
}

extern "C" fn send_input_buffer_for_decode(
    this: u64,
    index: i32,
    offset: i32,
    size: i32,
    pts: i64,
    flags: u32,
) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    o.decoder.send_input(index, offset, size, pts, flags)
}

extern "C" fn acquire_output_buffer_index(
    this: u64,
    index: u64,
    pts: u64,
    flags: u64,
    left: u64,
) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    if index == 0 || pts == 0 || flags == 0 || left == 0 {
        return ERR_BAD_ARG;
    }
    match o.decoder.acquire_output() {
        Ok((i, t, fl, n)) => {
            let ok = guest::write_i32(index, i).is_ok()
                && guest::write_i64(pts, t).is_ok()
                && guest::write_u32(flags, fl).is_ok()
                && guest::write_i32(left, n).is_ok();
            if !ok {
                return ERR_BAD_ARG;
            }
            pacing::picture();
            0
        }
        Err(code) => {
            let _ = guest::write_i32(index, -1);
            let _ = guest::write_i32(left, 0);
            code
        }
    }
}

/// Writes the format of the pictures into the game's `MediaData`.
fn write_format(o: &Object, media_data: u64, f: &Fns) {
    let fmt = o.decoder.format();
    let (w, h) = (fmt.width as i32, fmt.height as i32);
    let (stride, slice) = (w, h);
    let set = |key: &std::ffi::CStr, value: i32| {
        (f.set_int32)(media_data, key.as_ptr(), value);
    };
    set(c"width", w);
    set(c"height", h);
    (f.set_rect)(media_data, c"crop".as_ptr(), 0, 0, w - 1, h - 1);
    set(c"stride", stride);
    set(c"slice-height", slice);
    set(c"nv12-y-stride", stride);
    set(c"nv12-y-offset", 0);
    set(c"nv12-uv-offset", stride * slice);
    set(c"nv12-uv-stride", stride);
    set(c"nv12-width", w);
    set(c"nv12-height", h);
    set(c"nv12-colorspace", fmt.colorspace as i32);
}

fn format_call(this: u64, media_data: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    if media_data == 0 {
        return ERR_BAD_ARG;
    }
    match fns() {
        Ok(f) => {
            write_format(&o, media_data, f);
            0
        }
        Err(e) => {
            red(&e);
            ERR_NO_IMPL
        }
    }
}

extern "C" fn get_output_format(this: u64, media_data: u64) -> u32 {
    format_call(this, media_data)
}

extern "C" fn get_output_buffer_format(this: u64, _index: i32, media_data: u64) -> u32 {
    format_call(this, media_data)
}

extern "C" fn get_output_buffer_size(this: u64, size: u64, mode: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    if size == 0 || mode == 0 {
        return ERR_BAD_ARG;
    }
    let fmt = o.decoder.format();
    let bytes = fmt.width as i64 * fmt.height as i64 * 3 / 2;
    let reported = if o.out_align != 0 {
        o.out_align
    } else {
        o.mode
    };
    if guest::write_i32(size, bytes as i32).is_err() || guest::write_u32(mode, reported).is_err() {
        return ERR_BAD_ARG;
    }
    0
}

/// The picture goes into a buffer of the game as NV12 bytes. A slot without a picture gives black.
fn output_to_memory(o: &Object, buffer: u64, f: &Fns, index: usize) -> u32 {
    (f.set_range)(buffer, 0, 0);
    let fmt = o.decoder.format();
    let (stride, slice) = (fmt.width as usize, fmt.height as usize);
    let need = stride * slice * 3 / 2;
    let data = (f.data)(buffer);
    let capacity = (f.capacity)(buffer) as usize;
    if data == 0 || capacity < need {
        red(&format!(
            "the output buffer of the game holds {capacity} bytes, a picture needs {need}"
        ));
        return ERR_BAD_ARG;
    }
    let picture = o.decoder.picture(index);
    let copied = guest::with_slice_mut(data, need as u64, |dst| match &picture {
        Some(p) => p.copy_nv12(dst, stride, slice),
        None => {
            let (luma, chroma) = dst.split_at_mut(stride * slice);
            luma.fill(16);
            chroma.fill(128);
            Ok(())
        }
    });
    match copied {
        Ok(Ok(())) => {
            (f.set_range)(buffer, 0, need as i32);
            0
        }
        Ok(Err(e)) => {
            red(&format!("a picture cannot be copied: {e}"));
            ERR_BAD_ARG
        }
        Err(fault) => {
            red(&format!(
                "the output buffer of the game is not usable: {fault}"
            ));
            ERR_BAD_ARG
        }
    }
}

/// The picture goes into a texture of the game through the host's texture sink. The buffer's data is the texture
/// object itself. A slot without a picture leaves the texture as it was, so the last picture stays on screen.
fn output_to_texture(o: &Object, buffer: u64, f: &Fns, index: usize) -> u32 {
    let Some(sink) = config().and_then(|c| c.sink) else {
        return ERR_BAD_ARG;
    };
    let texture = (f.data)(buffer);
    let capacity = (f.capacity)(buffer);
    if texture == 0 || capacity < 8 {
        red(&format!(
            "the buffer of the game in native texture mode holds {capacity} bytes and needs 8 for a texture object"
        ));
        return ERR_BAD_ARG;
    }
    if let Some(p) = o.decoder.picture(index)
        && let Err(e) = sink.present(texture, &p)
    {
        red(&format!(
            "a picture cannot go into the texture at {texture:#x}: {e}"
        ));
        return ERR_BAD_ARG;
    }
    if let Some(nvn_sync) = f.nvn_sync {
        let sync = nvn_sync(buffer);
        if sync != 0
            && let Err(e) = sink.signal_sync(sync)
        {
            red(&format!(
                "the sync object at {sync:#x} cannot be signalled: {e}"
            ));
        }
    }
    // The SDK's texture mode hands the slot back inside this call. Slots rendered before go back now.
    o.decoder.rendered(index);
    0
}

fn output_buffer(this: u64, index: i32, buffer: u64, media_data: u64) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    if media_data != 0 {
        let _ = format_call(this, media_data);
    }
    if buffer == 0 || index < 0 || index as usize >= OUT_COUNT {
        return ERR_BAD_ARG;
    }
    let f = match fns() {
        Ok(f) => f,
        Err(e) => {
            red(&e);
            return ERR_NO_IMPL;
        }
    };
    if o.mode == MODE_TEXTURE {
        output_to_texture(&o, buffer, f, index as usize)
    } else {
        output_to_memory(&o, buffer, f, index as usize)
    }
}

extern "C" fn get_output_buffer(this: u64, index: i32, buffer: u64) -> u32 {
    output_buffer(this, index, buffer, 0)
}

extern "C" fn get_output_buffer_with_format(
    this: u64,
    index: i32,
    buffer: u64,
    media_data: u64,
) -> u32 {
    output_buffer(this, index, buffer, media_data)
}

extern "C" fn release_output_buffer_index(this: u64, index: i32) -> u32 {
    let Some(o) = object(this) else {
        return ERR_NO_IMPL;
    };
    if index < 0 || index as usize >= OUT_COUNT {
        return ERR_BAD_ARG;
    }
    o.decoder.release_output(index as usize);
    0
}

extern "C" fn get_last_error(_this: u64) -> u32 {
    0
}

/// The functions that this service gives the game.
pub(crate) fn table() -> Vec<Export> {
    vec![
        Export::new(
            "_ZN5movie7DecoderC1ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeEi",
            ctor_full as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC2ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeEi",
            ctor_full as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC1ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeE",
            ctor_no_priority as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC2ENS_11DecoderTypeENS_19DecoderOutputFormatENS_11DecoderModeE",
            ctor_no_priority as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC1ENS_11DecoderTypeENS_19DecoderOutputFormatE",
            ctor_sdk3 as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7DecoderC2ENS_11DecoderTypeENS_19DecoderOutputFormatE",
            ctor_sdk3 as *const () as usize,
        ),
        Export::new("_ZN5movie7DecoderD0Ev", dtor_delete as *const () as usize),
        Export::new("_ZN5movie7DecoderD1Ev", dtor as *const () as usize),
        Export::new("_ZN5movie7DecoderD2Ev", dtor as *const () as usize),
        Export::new(
            "_ZN5movie7Decoder9ConfigureEPNS_9MediaDataE",
            configure as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder20RegisterClientEventsEPNS_13DecoderEventsE",
            register_client_events as *const () as usize,
        ),
        Export::new("_ZN5movie7Decoder5StartEv", start as *const () as usize),
        Export::new("_ZN5movie7Decoder4StopEv", stop as *const () as usize),
        Export::new("_ZN5movie7Decoder5ResetEv", reset as *const () as usize),
        Export::new("_ZN5movie7Decoder5FlushEv", flush as *const () as usize),
        Export::new(
            "_ZN5movie7Decoder23AcquireInputBufferIndexEPiS1_",
            acquire_input_buffer_index as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder14GetInputBufferEiPNS_6BufferE",
            get_input_buffer as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder24SendInputBufferForDecodeEiiilj",
            send_input_buffer_for_decode as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder24AcquireOutputBufferIndexEPiPlPjS1_",
            acquire_output_buffer_index as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder15GetOutputFormatEPNS_9MediaDataE",
            get_output_format as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder21GetOutputBufferFormatEiPNS_9MediaDataE",
            get_output_buffer_format as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder19GetOutputBufferSizeEPiS1_",
            get_output_buffer_size as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder15GetOutputBufferEiPNS_6BufferE",
            get_output_buffer as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder15GetOutputBufferEiPNS_6BufferEPNS_9MediaDataE",
            get_output_buffer_with_format as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder24ReleaseOutputBufferIndexEi",
            release_output_buffer_index as *const () as usize,
        ),
        Export::new(
            "_ZN5movie7Decoder12GetLastErrorEv",
            get_last_error as *const () as usize,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_each_name_once() {
        let t = table();
        let mut names: Vec<&str> = t.iter().map(|e| e.name).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(
            t.iter()
                .all(|e| e.addr != 0 && e.name.starts_with("_ZN5movie7Decoder"))
        );
    }
}
