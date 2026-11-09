//! The SDK take-overs of `nn::audio::AudioOut` and `AudioIn`. They follow `hle_audio.c`.
//!
//! The game calls these functions in place of the SDK. The SDK would reach the `audout:u` service. Here the calls land on
//! the mixer directly. A guest `AudioOut` object is mapped to a mixer slot by its address.

use crate::engine::{self, Buf, Notify, mixer};
use crate::host;
use crate::util::{DEVICE_OUT, IdxMap, RES_FAIL, ex, put_name};
use nxkit_abi::export::Export;
use nxkit_abi::guest;
use std::sync::atomic::{AtomicBool, Ordering};

static OUTS: IdxMap = IdxMap::new();
static AI_LOGGED: AtomicBool = AtomicBool::new(false);

/// `AudioOutBuffer`: the data pointer at +0, the capacity at +8 and the data size at +0x10.
const BUF_PTR: u64 = 0;
const BUF_SIZE: u64 = 0x10;

/// Opens a mixer slot for `ch` channels. The event at `ev` (0 for none) is bound to a kernel event first.
pub(crate) fn open_slot(ev: u64, ch: u32) -> Option<usize> {
    let notify = if ev != 0 {
        host::bind_event(ev, true);
        Notify::Guest(ev)
    } else {
        Notify::None
    };
    mixer().open(ch, notify)
}

/// Writes what the SDK's own `OpenAudioOut` leaves in the front of the object: rate, channels, format and state.
/// Code outside the SDK reads these words.
fn write_info(g: u64, ch: u32, state: u32) {
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&engine::RATE.to_le_bytes());
    b[4..8].copy_from_slice(&ch.to_le_bytes());
    b[8..12].copy_from_slice(&2u32.to_le_bytes());
    b[12..16].copy_from_slice(&state.to_le_bytes());
    let _ = guest::write(g, &b);
}

fn open_at(g: u64, ev: u64, param: u64) -> u32 {
    let rate = guest::read_i32(param).unwrap_or(0);
    let mut ch = u32::from(guest::read_u16(param + 4).unwrap_or(0));
    if ch == 0 {
        ch = 2;
    }
    if rate != 0 && rate != engine::RATE as i32 {
        host::fatal(&format!(
            "AudioOut sample rate {rate} is not supported (48000 only)"
        ));
    }
    let Some(idx) = open_slot(ev, ch) else {
        host::fatal(&format!(
            "more than {} AudioOuts are open at once",
            engine::SLOTS
        ));
    };
    if !OUTS.put(g, idx) {
        host::fatal("more AudioOut objects than the host can map");
    }
    write_info(g, ch, engine::STOPPED);
    let how = if ev == 0 {
        ", no release event (the game polls)"
    } else {
        ""
    };
    eprintln!("nxkit-audio: OpenAudioOut {g:#x} with {ch} channels, slot {idx}{how}");
    0
}

fn slot(g: u64) -> usize {
    match OUTS.get(g) {
        Some(i) => i,
        None => host::fatal(&format!("AudioOut {g:#x} was not opened by the host")),
    }
}

extern "C" fn init_param(p: u64) {
    let _ = guest::write(p, &[0u8; 8]);
}

extern "C" fn list_outs(info: u64, n: i32) -> i32 {
    if n < 1 {
        return 0;
    }
    put_name(info, "DeviceOut", 256);
    1
}

extern "C" fn open_out(g: u64, ev: u64, _name: u64, param: u64) -> u32 {
    open_at(g, ev, param)
}

extern "C" fn open_out_no_event(g: u64, _name: u64, param: u64) -> u32 {
    open_at(g, 0, param)
}

extern "C" fn open_default(g: u64, ev: u64, param: u64) -> u32 {
    open_at(g, ev, param)
}

extern "C" fn open_default_no_event(g: u64, param: u64) -> u32 {
    open_at(g, 0, param)
}

extern "C" fn start(g: u64) -> u32 {
    mixer().start(slot(g));
    0
}

extern "C" fn stop(g: u64) {
    mixer().stop(slot(g));
}

extern "C" fn close(g: u64) {
    let idx = slot(g);
    let ch = mixer().channels(idx);
    mixer().close(idx);
    OUTS.del(g);
    write_info(g, ch, engine::STOPPED);
}

extern "C" fn state(g: u64) -> u32 {
    mixer().state(slot(g))
}

extern "C" fn buffer_count(g: u64) -> u32 {
    mixer().queued(slot(g))
}

extern "C" fn sample_rate(g: u64) -> i32 {
    let _ = slot(g);
    engine::RATE as i32
}

extern "C" fn channel_count(g: u64) -> i32 {
    mixer().channels(slot(g)) as i32
}

extern "C" fn name(_g: u64) -> u64 {
    DEVICE_OUT.as_ptr() as u64
}

extern "C" fn sample_format(g: u64) -> i32 {
    let _ = slot(g);
    2
}

extern "C" fn set_volume(g: u64, v: f32) {
    mixer().set_volume(slot(g), v);
}

extern "C" fn get_volume(g: u64) -> f32 {
    mixer().volume(slot(g))
}

extern "C" fn set_buffer_info(b: u64, p: u64, cap: u64, size: u64) {
    let mut v = [0u8; 24];
    v[0..8].copy_from_slice(&p.to_le_bytes());
    v[8..16].copy_from_slice(&cap.to_le_bytes());
    v[16..24].copy_from_slice(&size.to_le_bytes());
    let _ = guest::write(b, &v);
}

extern "C" fn buffer_data_pointer(b: u64) -> u64 {
    guest::read_u64(b + BUF_PTR).unwrap_or(0)
}

extern "C" fn buffer_data_size(b: u64) -> u64 {
    guest::read_u64(b + BUF_SIZE).unwrap_or(0)
}

extern "C" fn append(g: u64, b: u64) -> u32 {
    let idx = slot(g);
    let addr = guest::read_u64(b + BUF_PTR).unwrap_or(0);
    let size = guest::read_u64(b + BUF_SIZE).unwrap_or(0);
    u32::from(mixer().append(
        idx,
        Buf {
            token: b,
            addr,
            size,
            host: false,
        },
    ))
}

extern "C" fn released(g: u64) -> u64 {
    mixer().released(slot(g)).map_or(0, |b| b.token)
}

fn ai_open(name: u64) -> u32 {
    if !AI_LOGGED.swap(true, Ordering::Relaxed) {
        let n = if name == 0 {
            Vec::new()
        } else {
            guest::read_cstr(name, 64).unwrap_or_default()
        };
        eprintln!(
            "nxkit-audio: audio input {}: the host has no microphone, the open fails",
            if n.is_empty() {
                "(default)".to_string()
            } else {
                String::from_utf8_lossy(&n).into_owned()
            }
        );
    }
    RES_FAIL
}

extern "C" fn list_ins(_info: u64, _n: i32) -> i32 {
    0
}

extern "C" fn open_in(_g: u64, _ev: u64, name: u64, _param: u64) -> u32 {
    ai_open(name)
}

extern "C" fn open_in_no_event(_g: u64, name: u64, _param: u64) -> u32 {
    ai_open(name)
}

extern "C" fn open_default_in(_g: u64, _ev: u64, _param: u64) -> u32 {
    ai_open(0)
}

extern "C" fn start_in(_g: u64) -> u32 {
    RES_FAIL
}

extern "C" fn nothing1(_g: u64) {}

extern "C" fn append_in(_g: u64, _b: u64) -> u32 {
    0
}

extern "C" fn released_in(_g: u64) -> u64 {
    0
}

extern "C" fn in_rate(_g: u64) -> i32 {
    engine::RATE as i32
}

extern "C" fn in_channels(_g: u64) -> i32 {
    2
}

extern "C" fn in_format(_g: u64) -> i32 {
    2
}

/// Adds the exports of this module. `StartAudioOut`, `StopAudioOut` and `CloseAudioOut` go to `big` because Core Audio
/// needs far more stack than a game thread may have left.
pub(crate) fn exports(v: &mut Vec<Export>, big: &mut Vec<Export>) {
    ex!(
        v,
        "_ZN2nn5audio27InitializeAudioOutParameterEPNS0_17AudioOutParameterE",
        init_param
    );
    ex!(
        v,
        "_ZN2nn5audio13ListAudioOutsEPNS0_12AudioOutInfoEi",
        list_outs
    );
    ex!(
        v,
        "_ZN2nn5audio12OpenAudioOutEPNS0_8AudioOutEPNS_2os11SystemEventEPKcRKNS0_17AudioOutParameterE",
        open_out
    );
    ex!(
        v,
        "_ZN2nn5audio12OpenAudioOutEPNS0_8AudioOutEPKcRKNS0_17AudioOutParameterE",
        open_out_no_event
    );
    ex!(
        v,
        "_ZN2nn5audio19OpenDefaultAudioOutEPNS0_8AudioOutEPNS_2os11SystemEventERKNS0_17AudioOutParameterE",
        open_default
    );
    ex!(
        v,
        "_ZN2nn5audio19OpenDefaultAudioOutEPNS0_8AudioOutERKNS0_17AudioOutParameterE",
        open_default_no_event
    );
    ex!(big, "_ZN2nn5audio13StartAudioOutEPNS0_8AudioOutE", start);
    ex!(big, "_ZN2nn5audio12StopAudioOutEPNS0_8AudioOutE", stop);
    ex!(big, "_ZN2nn5audio13CloseAudioOutEPNS0_8AudioOutE", close);
    ex!(v, "_ZN2nn5audio16GetAudioOutStateEPKNS0_8AudioOutE", state);
    ex!(
        v,
        "_ZN2nn5audio22GetAudioOutBufferCountEPKNS0_8AudioOutE",
        buffer_count
    );
    ex!(
        v,
        "_ZN2nn5audio21GetAudioOutSampleRateEPKNS0_8AudioOutE",
        sample_rate
    );
    ex!(
        v,
        "_ZN2nn5audio23GetAudioOutChannelCountEPKNS0_8AudioOutE",
        channel_count
    );
    ex!(v, "_ZN2nn5audio15GetAudioOutNameEPKNS0_8AudioOutE", name);
    ex!(
        v,
        "_ZN2nn5audio23GetAudioOutSampleFormatEPKNS0_8AudioOutE",
        sample_format
    );
    ex!(
        v,
        "_ZN2nn5audio17SetAudioOutVolumeEPNS0_8AudioOutEf",
        set_volume
    );
    ex!(
        v,
        "_ZN2nn5audio17GetAudioOutVolumeEPKNS0_8AudioOutE",
        get_volume
    );
    ex!(
        v,
        "_ZN2nn5audio21SetAudioOutBufferInfoEPNS0_14AudioOutBufferEPvmm",
        set_buffer_info
    );
    ex!(
        v,
        "_ZN2nn5audio28GetAudioOutBufferDataPointerEPKNS0_14AudioOutBufferE",
        buffer_data_pointer
    );
    ex!(
        v,
        "_ZN2nn5audio25GetAudioOutBufferDataSizeEPKNS0_14AudioOutBufferE",
        buffer_data_size
    );
    ex!(
        v,
        "_ZN2nn5audio20AppendAudioOutBufferEPNS0_8AudioOutEPNS0_14AudioOutBufferE",
        append
    );
    ex!(
        v,
        "_ZN2nn5audio25GetReleasedAudioOutBufferEPNS0_8AudioOutE",
        released
    );

    ex!(
        v,
        "_ZN2nn5audio12ListAudioInsEPNS0_11AudioInInfoEi",
        list_ins
    );
    ex!(
        v,
        "_ZN2nn5audio11OpenAudioInEPNS0_7AudioInEPNS_2os11SystemEventEPKcRKNS0_16AudioInParameterE",
        open_in
    );
    ex!(
        v,
        "_ZN2nn5audio11OpenAudioInEPNS0_7AudioInEPKcRKNS0_16AudioInParameterE",
        open_in_no_event
    );
    ex!(
        v,
        "_ZN2nn5audio18OpenDefaultAudioInEPNS0_7AudioInEPNS_2os11SystemEventERKNS0_16AudioInParameterE",
        open_default_in
    );
    ex!(v, "_ZN2nn5audio12StartAudioInEPNS0_7AudioInE", start_in);
    ex!(v, "_ZN2nn5audio11StopAudioInEPNS0_7AudioInE", nothing1);
    ex!(v, "_ZN2nn5audio12CloseAudioInEPNS0_7AudioInE", nothing1);
    ex!(
        v,
        "_ZN2nn5audio19AppendAudioInBufferEPNS0_7AudioInEPNS0_13AudioInBufferE",
        append_in
    );
    ex!(
        v,
        "_ZN2nn5audio24GetReleasedAudioInBufferEPNS0_7AudioInE",
        released_in
    );
    ex!(
        v,
        "_ZN2nn5audio20GetAudioInSampleRateEPKNS0_7AudioInE",
        in_rate
    );
    ex!(
        v,
        "_ZN2nn5audio22GetAudioInChannelCountEPKNS0_7AudioInE",
        in_channels
    );
    ex!(
        v,
        "_ZN2nn5audio22GetAudioInSampleFormatEPKNS0_7AudioInE",
        in_format
    );
}
