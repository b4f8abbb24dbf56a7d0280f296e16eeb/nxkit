//! The SDK take-overs of the SDK 21 `OutputStream` and `InputStream`. They follow `hle_audio.c` of nxnative.
//!
//! A stream owns one mixer output. Each write becomes one 16-bit interleaved block that the host owns. The free count
//! is the capacity less the frames still queued, and the release event of the stream is the one of the output.
//!
//! A call finds its stream through an [`IdxMap`] and then the state of that mixer slot. The state has its own lock,
//! which only the game thread that calls the stream takes. No call allocates once the stream has been written to as
//! many times as it queues blocks: played blocks go to a free list and the next write reuses them.

use crate::engine::{self, Buf, QMAX, SLOTS, mixer};
use crate::host;
use crate::sdk_out::open_slot;
use crate::util::{IdxMap, RES_FAIL, ex, put_name};
use nxkit_abi::export::Export;
use nxkit_abi::guest;
use std::sync::Mutex;

const FMT_PCM16: i32 = 2;
const FMT_FLOAT: i32 = 5;
const ST_STOPPED: u32 = 1;
const ST_STARTED: u32 = 2;

struct Block {
    data: Vec<u8>,
    frames: i32,
}

struct Inner {
    ch: usize,
    fmt: i32,
    cap: i32,
    queued: i32,
    state: u32,
    /// Queued blocks in write order. The token of a block is the address of its data, which a move of the `Vec` keeps.
    blocks: Vec<Block>,
    /// Played blocks whose buffers the next write reuses.
    spare: Vec<Vec<u8>>,
}

/// The guest address of an `OutputStream` to its mixer slot.
static STREAMS: IdxMap = IdxMap::new();

/// The state of the stream on each mixer slot.
static POOL: [Mutex<Option<Inner>>; SLOTS] = [const { Mutex::new(None) }; SLOTS];

/// Runs `f` on the stream at `g` and its mixer slot. A stream that the host did not open, or a poisoned lock, stops the
/// game, since both are bugs.
fn with<R>(g: u64, f: impl FnOnce(usize, &mut Inner) -> R) -> R {
    let Some(idx) = STREAMS.get(g) else {
        host::fatal(&format!("OutputStream {g:#x} was not opened by the host"));
    };
    match POOL[idx].lock() {
        Ok(mut s) => match s.as_mut() {
            Some(i) => f(idx, i),
            None => host::fatal(&format!("OutputStream {g:#x} is closed")),
        },
        Err(_) => host::fatal("an OutputStream lock was poisoned"),
    }
}

/// Takes back the blocks that the mixer played.
fn reclaim(idx: usize, i: &mut Inner) {
    while let Some(b) = mixer().released(idx) {
        if let Some(k) = i
            .blocks
            .iter()
            .position(|k| k.data.as_ptr() as u64 == b.token)
        {
            let k = i.blocks.remove(k);
            i.queued -= k.frames;
            i.spare.push(k.data);
        }
    }
}

fn open_at(g: u64, ev: u64, fmt: u64, frames: i32) -> u32 {
    let f = guest::read_i32(fmt).unwrap_or(0);
    let ch = guest::read_i32(fmt + 4).unwrap_or(0);
    let rate = guest::read_i32(fmt + 8).unwrap_or(0);
    if f != FMT_FLOAT && f != FMT_PCM16 {
        eprintln!(
            "nxkit-audio: [RED] OutputStream sample format {f} is not supported, the open fails"
        );
        return RES_FAIL;
    }
    if rate != 0 && rate != engine::RATE as i32 {
        eprintln!(
            "nxkit-audio: [RED] OutputStream sample rate {rate} is not supported (48000 only), the open fails"
        );
        return RES_FAIL;
    }
    if ch <= 0 || frames <= 0 {
        return RES_FAIL;
    }
    let Some(idx) = open_slot(ev, ch as u32) else {
        host::fatal(&format!(
            "more than {} audio outputs are open at once",
            engine::SLOTS
        ));
    };
    let inner = Inner {
        ch: ch as usize,
        fmt: f,
        cap: frames,
        queued: 0,
        state: ST_STOPPED,
        blocks: Vec::with_capacity(QMAX),
        spare: Vec::with_capacity(QMAX),
    };
    match POOL[idx].lock() {
        Ok(mut s) => *s = Some(inner),
        Err(_) => host::fatal("an OutputStream lock was poisoned"),
    }
    if !STREAMS.put(g, idx) {
        host::fatal("too many OutputStreams are open");
    }
    eprintln!(
        "nxkit-audio: OpenOutputStream {g:#x} with {ch} channels, {frames} frames, slot {idx}"
    );
    0
}

extern "C" fn default_device(name: u64) {
    put_name(name, "AudioVirtualOutput", 0x100);
}

#[allow(clippy::too_many_arguments)]
extern "C" fn open_stream(
    g: u64,
    ev: u64,
    _buf: u64,
    _size: u64,
    _dev: u64,
    fmt: u64,
    frames: i32,
    _threshold: i32,
) -> u32 {
    open_at(g, ev, fmt, frames)
}

extern "C" fn open_stream_no_event(
    g: u64,
    _buf: u64,
    _size: u64,
    _dev: u64,
    fmt: u64,
    frames: i32,
) -> u32 {
    open_at(g, 0, fmt, frames)
}

extern "C" fn start_stream(g: u64) -> u32 {
    with(g, |idx, i| {
        i.state = ST_STARTED;
        mixer().start(idx);
    });
    0
}

extern "C" fn start_streams(gs: u64, n: i32) -> u32 {
    for k in 0..n.max(0) as u64 {
        let g = guest::read_u64(gs + 8 * k).unwrap_or(0);
        start_stream(g);
    }
    0
}

extern "C" fn stop_stream(g: u64) {
    with(g, |idx, i| {
        mixer().stop(idx);
        i.state = ST_STOPPED;
    });
}

extern "C" fn purge_stream(g: u64) {
    with(g, |idx, i| {
        reclaim(idx, i);
        mixer().purge(idx);
        reclaim(idx, i);
    });
}

extern "C" fn close_stream(g: u64) {
    let Some(idx) = STREAMS.get(g) else { return };
    with(g, |idx, i| {
        mixer().stop(idx);
        reclaim(idx, i);
        mixer().close(idx);
    });
    if let Ok(mut s) = POOL[idx].lock() {
        *s = None;
    }
    STREAMS.del(g);
}

extern "C" fn stream_state(g: u64) -> u32 {
    if STREAMS.get(g).is_none() {
        return 0;
    }
    with(g, |_, i| i.state)
}

extern "C" fn free_frames(g: u64) -> i32 {
    with(g, |idx, i| {
        reclaim(idx, i);
        let n = if i.blocks.len() >= QMAX - 1 {
            0
        } else {
            i.cap - i.queued
        };
        n.max(0)
    })
}

extern "C" fn write_stream(g: u64, data: u64, frames: i32) -> i32 {
    if frames <= 0 || data == 0 {
        return 0;
    }
    with(g, |idx, i| {
        reclaim(idx, i);
        if i.blocks.len() >= QMAX - 1 || frames > i.cap - i.queued {
            return 0;
        }
        let n = frames as usize * i.ch;
        let mut pcm = i.spare.pop().unwrap_or_default();
        pcm.clear();
        pcm.resize(n * 2, 0);
        if i.fmt == FMT_FLOAT {
            let ok = guest::with_slice(data, n as u64 * 4, |src| {
                for (k, c) in src.as_chunks::<4>().0.iter().enumerate() {
                    let v = f32::from_le_bytes(*c) * 32767.0;
                    let q = v.clamp(-32768.0, 32767.0) as i16;
                    pcm[2 * k..2 * k + 2].copy_from_slice(&q.to_le_bytes());
                }
            })
            .is_ok();
            if !ok {
                i.spare.push(pcm);
                return 0;
            }
        } else if guest::read(data, &mut pcm).is_err() {
            i.spare.push(pcm);
            return 0;
        }
        let addr = pcm.as_ptr() as u64;
        let size = pcm.len() as u64;
        i.blocks.push(Block { data: pcm, frames });
        if !mixer().append(
            idx,
            Buf {
                token: addr,
                addr,
                size,
                host: true,
            },
        ) {
            if let Some(b) = i.blocks.pop() {
                i.spare.push(b.data);
            }
            return 0;
        }
        i.queued += frames;
        frames
    })
}

extern "C" fn set_gain(g: u64, gain: f32, _fade: i64) {
    with(g, |idx, _| mixer().set_volume(idx, gain));
}

extern "C" fn filled_frames(g: u64) -> i32 {
    with(g, |idx, i| {
        reclaim(idx, i);
        i.queued
    })
}

/// The statistics of a stream: 24 bytes, returned in memory. The game treats a count above zero as a glitch.
#[repr(C)]
#[allow(dead_code)]
struct Stats {
    a: i64,
    b: i64,
    c: i64,
}

extern "C" fn statistics(_g: u64, _reset: u32) -> Stats {
    Stats { a: 0, b: 0, c: 0 }
}

extern "C" fn list_inputs(_names: u64, _n: i32) -> i32 {
    0
}

extern "C" fn input_props(_props: u64, _name: u64) -> u32 {
    RES_FAIL
}

extern "C" fn open_input(
    _g: u64,
    _buf: u64,
    _size: u64,
    _dev: u64,
    _fmt: u64,
    _frames: i32,
) -> u32 {
    RES_FAIL
}

extern "C" fn input_nothing(_g: u64) {}

extern "C" fn input_start(_g: u64) -> u32 {
    RES_FAIL
}

extern "C" fn input_read(_g: u64, _buf: u64, _frames: i32) -> i32 {
    0
}

extern "C" fn input_filled(_g: u64) -> i32 {
    0
}

/// The SDK passes the calls below to the `Impl` pointer of the stream, which is null for a type that never opened. Only
/// `GetInputStreamState` answers a null `Impl` itself. The Legend of Zelda: Ocarina of Time asks the state of its two
/// microphone streams, purges them when they are not started and reads their statistics each update. It opens them only
/// when a virtual input device is listed, so here never. Each call answers without touching the guest object.
///
/// Input stream, never open: closed (0).
extern "C" fn input_state(_g: u64) -> i32 {
    0
}

/// Input stream, never open: nothing buffered.
extern "C" fn input_purge(_g: u64) {}

/// Input stream, never open: none.
extern "C" fn input_count(_g: u64) -> i32 {
    0
}

/// Input stream, never open: 1.
extern "C" fn input_get_gain(_g: u64) -> f32 {
    1.0
}

/// Input stream, never open: nothing to ramp.
extern "C" fn input_set_gain(_g: u64, _gain: f32, _ramp: i64) {}

/// `StreamFormat`: 12 bytes, returned in registers.
#[repr(C)]
#[allow(dead_code)]
struct Format {
    format: i32,
    channels: i32,
    rate: i32,
}

extern "C" fn input_format(_g: u64) -> Format {
    Format {
        format: FMT_PCM16,
        channels: 1,
        rate: engine::RATE as i32,
    }
}

/// Adds the exports of this module. The start, stop and close functions go to `big`.
pub(crate) fn exports(v: &mut Vec<Export>, big: &mut Vec<Export>) {
    ex!(
        v,
        "_ZN2nn5audio29GetDefaultVirtualOutputDeviceEPNS0_15AudioDeviceNameE",
        default_device
    );
    ex!(
        v,
        "_ZN2nn5audio16OpenOutputStreamEPNS0_16OutputStreamTypeEPNS_2os11SystemEventEPvmRKNS0_15AudioDeviceNameERKNS0_12StreamFormatEiiNS_4util10BitFlagSetILi32ENS0_16OutputStreamFlagEEE",
        open_stream
    );
    ex!(
        v,
        "_ZN2nn5audio16OpenOutputStreamEPNS0_16OutputStreamTypeEPvmRKNS0_15AudioDeviceNameERKNS0_12StreamFormatEiNS_4util10BitFlagSetILi32ENS0_16OutputStreamFlagEEE",
        open_stream_no_event
    );
    ex!(
        big,
        "_ZN2nn5audio17StartOutputStreamEPNS0_16OutputStreamTypeE",
        start_stream
    );
    ex!(
        big,
        "_ZN2nn5audio18StartOutputStreamsEPKPNS0_16OutputStreamTypeEi",
        start_streams
    );
    ex!(
        big,
        "_ZN2nn5audio16StopOutputStreamEPNS0_16OutputStreamTypeE",
        stop_stream
    );
    ex!(
        v,
        "_ZN2nn5audio17PurgeOutputStreamEPNS0_16OutputStreamTypeE",
        purge_stream
    );
    ex!(
        big,
        "_ZN2nn5audio17CloseOutputStreamEPNS0_16OutputStreamTypeE",
        close_stream
    );
    ex!(
        v,
        "_ZN2nn5audio20GetOutputStreamStateEPKNS0_16OutputStreamTypeE",
        stream_state
    );
    ex!(
        v,
        "_ZN2nn5audio29GetOutputStreamFreeFrameCountEPKNS0_16OutputStreamTypeE",
        free_frames
    );
    ex!(
        v,
        "_ZN2nn5audio17WriteOutputStreamEPNS0_16OutputStreamTypeEPKvi",
        write_stream
    );
    ex!(
        v,
        "_ZN2nn5audio19SetOutputStreamGainEPNS0_16OutputStreamTypeEfNS_8TimeSpanE",
        set_gain
    );
    ex!(
        v,
        "_ZN2nn5audio31GetOutputStreamFilledFrameCountEPKNS0_16OutputStreamTypeE",
        filled_frames
    );
    ex!(
        v,
        "_ZN2nn5audio25GetOutputStreamStatisticsEPNS0_16OutputStreamTypeEb",
        statistics
    );
    ex!(
        v,
        "_ZN2nn5audio23ListVirtualInputDevicesEPNS0_15AudioDeviceNameEi",
        list_inputs
    );
    ex!(
        v,
        "_ZN2nn5audio31GetVirtualInputDevicePropertiesEPNS0_23VirtualDevicePropertiesERKNS0_15AudioDeviceNameE",
        input_props
    );
    ex!(
        v,
        "_ZN2nn5audio15OpenInputStreamEPNS0_15InputStreamTypeEPvmRKNS0_15AudioDeviceNameERKNS0_12StreamFormatEi",
        open_input
    );
    ex!(
        v,
        "_ZN2nn5audio16CloseInputStreamEPNS0_15InputStreamTypeE",
        input_nothing
    );
    ex!(
        v,
        "_ZN2nn5audio16StartInputStreamEPNS0_15InputStreamTypeE",
        input_start
    );
    ex!(
        v,
        "_ZN2nn5audio15StopInputStreamEPNS0_15InputStreamTypeE",
        input_nothing
    );
    ex!(
        v,
        "_ZN2nn5audio15ReadInputStreamEPNS0_15InputStreamTypeEPvi",
        input_read
    );
    ex!(
        v,
        "_ZN2nn5audio30GetInputStreamFilledFrameCountEPKNS0_15InputStreamTypeE",
        input_filled
    );
    ex!(
        v,
        "_ZN2nn5audio20GetInputStreamFormatEPKNS0_15InputStreamTypeE",
        input_format
    );
    ex!(
        v,
        "_ZN2nn5audio19GetInputStreamStateEPKNS0_15InputStreamTypeE",
        input_state
    );
    ex!(
        v,
        "_ZN2nn5audio16PurgeInputStreamEPNS0_15InputStreamTypeE",
        input_purge
    );
    ex!(
        v,
        "_ZN2nn5audio24GetInputStreamStatisticsEPNS0_15InputStreamTypeEb",
        statistics
    );
    ex!(
        v,
        "_ZN2nn5audio28GetInputStreamFreeFrameCountEPKNS0_15InputStreamTypeE",
        input_count
    );
    ex!(
        v,
        "_ZN2nn5audio32GetInputStreamCapacityFrameCountEPKNS0_15InputStreamTypeE",
        input_count
    );
    ex!(
        v,
        "_ZN2nn5audio18GetInputStreamGainEPKNS0_15InputStreamTypeE",
        input_get_gain
    );
    ex!(
        v,
        "_ZN2nn5audio18SetInputStreamGainEPNS0_15InputStreamTypeEfNS_8TimeSpanE",
        input_set_gain
    );
}
