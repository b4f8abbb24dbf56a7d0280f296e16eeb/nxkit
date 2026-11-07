//! `nn::codec::HardwareOpusDecoder` and `HardwareOpusMultiStreamDecoder` take-overs. Source: nxnative
//! `dm/host/hle_codec.c`.
//!
//! The console decodes Opus in hardware through the `hwopus` service. The host has no such service, and the SDK bundles
//! a software Opus decoder. Every function here calls the software function of the game through `guest_sym`, with the
//! software decoder state in place of the hardware one. The class forms share the virtual interface of the software
//! classes (the vtable slots match one by one), so their constructors build the software object in the same memory.
//!
//! The C forms keep the software state in a block of the host. The first 8 bytes of the game's decoder object hold the
//! address of the block, so a decode call finds its state with one load and no table. A finalized block goes to a
//! free list of 64 slots and the next decoder takes it again. nxnative keeps the state in a map under a lock.
//! The size of the game's type does not matter, because the type is only an 8-byte handle here.
//!
//! The SDK 21 asynchronous decoder manager (`InitializeHardwareOpusDecoderManager` and the enqueue and execute
//! functions) is not taken over.

use super::{fatal, get_u64, put_u64, put_zero};
use crate::event::guest_alloc;
use crate::seams::guest_sym;
use crate::sys::call_guest;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// The bytes of a software state. The SDK types are at most 0x38 bytes, so a page is far more than needed.
const STATE: u64 = 0x1000;
const FREE: usize = 64;

static FREE_LIST: [AtomicU64; FREE] = [const { AtomicU64::new(0) }; FREE];

/// The address of a function of the game, looked up once.
macro_rules! soft {
    ($fn:ident, $name:literal) => {
        fn $fn() -> u64 {
            static CELL: OnceLock<Option<u64>> = OnceLock::new();
            match CELL.get_or_init(|| guest_sym($name)) {
                Some(a) => *a,
                None => fatal(concat!("the game lacks ", $name)),
            }
        }
    };
}

soft!(soft_ctor, "_ZN2nn5codec11OpusDecoderC1Ev");
soft!(soft_work_size, "_ZN2nn5codec28GetOpusDecoderWorkBufferSizeEii");
soft!(soft_init, "_ZN2nn5codec21InitializeOpusDecoderEPNS0_15OpusDecoderTypeEiiPvm");
soft!(soft_fini, "_ZN2nn5codec19FinalizeOpusDecoderEPNS0_15OpusDecoderTypeE");
soft!(soft_rate, "_ZN2nn5codec24GetOpusDecoderSampleRateEPKNS0_15OpusDecoderTypeE");
soft!(soft_channels, "_ZN2nn5codec26GetOpusDecoderChannelCountEPKNS0_15OpusDecoderTypeE");
soft!(soft_reset, "_ZN2nn5codec16ResetOpusDecoderEPNS0_15OpusDecoderTypeE");
soft!(soft_decode, "_ZN2nn5codec21DecodeOpusInterleavedEPNS0_15OpusDecoderTypeEPmPiPsmPKvm");
soft!(ms_ctor, "_ZN2nn5codec22OpusMultiStreamDecoderC1Ev");
soft!(ms_work_size, "_ZN2nn5codec39GetOpusMultiStreamDecoderWorkBufferSizeEiiii");
soft!(ms_init, "_ZN2nn5codec32InitializeOpusMultiStreamDecoderEPNS0_26OpusMultiStreamDecoderTypeEiiiiPKhPvm");
soft!(ms_fini, "_ZN2nn5codec30FinalizeOpusMultiStreamDecoderEPNS0_26OpusMultiStreamDecoderTypeE");
soft!(ms_rate, "_ZN2nn5codec35GetOpusMultiStreamDecoderSampleRateEPKNS0_26OpusMultiStreamDecoderTypeE");
soft!(ms_channels, "_ZN2nn5codec37GetOpusMultiStreamDecoderChannelCountEPKNS0_26OpusMultiStreamDecoderTypeE");
soft!(ms_total, "_ZN2nn5codec41GetOpusMultiStreamDecoderTotalStreamCountEPKNS0_26OpusMultiStreamDecoderTypeE");
soft!(ms_stereo, "_ZN2nn5codec42GetOpusMultiStreamDecoderStereoStreamCountEPKNS0_26OpusMultiStreamDecoderTypeE");
soft!(ms_reset, "_ZN2nn5codec27ResetOpusMultiStreamDecoderEPNS0_26OpusMultiStreamDecoderTypeE");
soft!(ms_decode, "_ZN2nn5codec32DecodeOpusMultiStreamInterleavedEPNS0_26OpusMultiStreamDecoderTypeEPmPiPsmPKvm");

/// A block for a new decoder: one from the free list, else a new one.
fn take_block() -> u64 {
    for slot in &FREE_LIST {
        let b = slot.swap(0, Ordering::AcqRel);
        if b != 0 {
            put_zero(b, STATE);
            return b;
        }
    }
    let b = guest_alloc(STATE);
    if b == 0 {
        fatal("no memory for an Opus decoder state");
    }
    b
}

/// Gives a block back. A full list drops the block, which stays allocated.
fn give_block(b: u64) {
    for slot in &FREE_LIST {
        if slot.compare_exchange(0, b, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return;
        }
    }
}

/// The software state of a game decoder object. An object that was never initialized stops the game.
fn state_of(hw: u64) -> u64 {
    let s = get_u64(hw);
    if s == 0 {
        fatal(&format!("Opus decoder {hw:#x} was not initialized by the host"));
    }
    s
}

fn make_state(hw: u64) -> u64 {
    let s = take_block();
    put_u64(hw, s);
    s
}

fn drop_state(hw: u64) -> u64 {
    let s = get_u64(hw);
    if s != 0 {
        put_u64(hw, 0);
    }
    s
}

/// `HardwareOpusDecoder::HardwareOpusDecoder(int)`: the software decoder is built in the same memory.
extern "C" fn construct(this: u64) {
    call_guest(soft_ctor(), [this, 0, 0, 0, 0, 0]);
}

/// `GetHardwareOpusDecoderWorkBufferSize(int rate, int channels[, int option])`.
extern "C" fn work_size(rate: u32, channels: u32) -> u64 {
    call_guest(soft_work_size(), [u64::from(rate), u64::from(channels), 0, 0, 0, 0])
}

/// `InitializeHardwareOpusDecoder(type*, rate, channels, work, size, option)`.
extern "C" fn initialize(hw: u64, rate: u32, channels: u32, work: u64, size: u64) -> u32 {
    let s = get_u64(hw);
    let s = if s == 0 { make_state(hw) } else { s };
    call_guest(soft_init(), [s, u64::from(rate), u64::from(channels), work, size, 0]) as u32
}

extern "C" fn finalize(hw: u64) {
    let s = drop_state(hw);
    if s != 0 {
        call_guest(soft_fini(), [s, 0, 0, 0, 0, 0]);
        give_block(s);
    }
}

extern "C" fn sample_rate(hw: u64) -> u32 {
    call_guest(soft_rate(), [state_of(hw), 0, 0, 0, 0, 0]) as u32
}

extern "C" fn channel_count(hw: u64) -> u32 {
    call_guest(soft_channels(), [state_of(hw), 0, 0, 0, 0, 0]) as u32
}

extern "C" fn reset(hw: u64) {
    call_guest(soft_reset(), [state_of(hw), 0, 0, 0, 0, 0]);
}

/// `DecodeOpusInterleavedWithHardware(type*, size_t* consumed, int* samples, short* out, size_t out_size, const void* in,
/// size_t in_size)`.
extern "C" fn decode(hw: u64, consumed: u64, samples: u64, out: u64, out_size: u64, input: u64, in_size: u64) -> u32 {
    let args = [state_of(hw), consumed, samples, out, out_size, input];
    // The seventh argument goes in the next register of the software call, so the call has seven arguments.
    decode_with(soft_decode(), args, in_size)
}

/// A software call with seven arguments, which `call_guest` (six) cannot make.
fn decode_with(f: u64, a: [u64; 6], seventh: u64) -> u32 {
    crate::sys::call_guest7(f, a, seventh) as u32
}

/// `HardwareOpusMultiStreamDecoder::HardwareOpusMultiStreamDecoder(int)`.
extern "C" fn ms_construct(this: u64) {
    call_guest(ms_ctor(), [this, 0, 0, 0, 0, 0]);
}

/// `GetHardwareOpusMultiStreamDecoderWorkBufferSize(rate, channels, streams, stereo[, option])`.
extern "C" fn ms_work(rate: u32, channels: u32, streams: u32, stereo: u32) -> u64 {
    call_guest(ms_work_size(), [u64::from(rate), u64::from(channels), u64::from(streams), u64::from(stereo), 0, 0])
}

/// `InitializeHardwareOpusMultiStreamDecoder(type*, rate, channels, streams, stereo, mapping, work, size[, option])`.
/// The ninth argument, the option, is on the stack and unused.
extern "C" fn ms_initialize(hw: u64, rate: u32, channels: u32, streams: u32, stereo: u32, mapping: u64, work: u64, size: u64) -> u32 {
    let s = get_u64(hw);
    let s = if s == 0 { make_state(hw) } else { s };
    crate::sys::call_guest8(
        ms_init(),
        [s, u64::from(rate), u64::from(channels), u64::from(streams), u64::from(stereo), mapping],
        [work, size],
    ) as u32
}

extern "C" fn ms_finalize(hw: u64) {
    let s = drop_state(hw);
    if s != 0 {
        call_guest(ms_fini(), [s, 0, 0, 0, 0, 0]);
        give_block(s);
    }
}

macro_rules! ms_getter {
    ($name:ident, $soft:ident) => {
        extern "C" fn $name(hw: u64) -> u32 {
            call_guest($soft(), [state_of(hw), 0, 0, 0, 0, 0]) as u32
        }
    };
}

ms_getter!(ms_sample_rate, ms_rate);
ms_getter!(ms_channel_count, ms_channels);
ms_getter!(ms_total_streams, ms_total);
ms_getter!(ms_stereo_streams, ms_stereo);

extern "C" fn ms_reset_decoder(hw: u64) {
    call_guest(ms_reset(), [state_of(hw), 0, 0, 0, 0, 0]);
}

extern "C" fn ms_decode_frame(hw: u64, consumed: u64, samples: u64, out: u64, out_size: u64, input: u64, in_size: u64) -> u32 {
    decode_with(ms_decode(), [state_of(hw), consumed, samples, out, out_size, input], in_size)
}

table! {
    "_ZN2nn5codec19HardwareOpusDecoderC1Ei" => construct,
    "_ZN2nn5codec36GetHardwareOpusDecoderWorkBufferSizeEiii" => work_size,
    "_ZN2nn5codec36GetHardwareOpusDecoderWorkBufferSizeEii" => work_size,
    "_ZN2nn5codec29InitializeHardwareOpusDecoderEPNS0_23HardwareOpusDecoderTypeEiiPvmi" => initialize,
    "_ZN2nn5codec27FinalizeHardwareOpusDecoderEPNS0_23HardwareOpusDecoderTypeE" => finalize,
    "_ZN2nn5codec32GetHardwareOpusDecoderSampleRateEPKNS0_23HardwareOpusDecoderTypeE" => sample_rate,
    "_ZN2nn5codec34GetHardwareOpusDecoderChannelCountEPKNS0_23HardwareOpusDecoderTypeE" => channel_count,
    "_ZN2nn5codec24ResetHardwareOpusDecoderEPNS0_23HardwareOpusDecoderTypeE" => reset,
    "_ZN2nn5codec33DecodeOpusInterleavedWithHardwareEPNS0_23HardwareOpusDecoderTypeEPmPiPsmPKvm" => decode,
    "_ZN2nn5codec30HardwareOpusMultiStreamDecoderC1Ei" => ms_construct,
    "_ZN2nn5codec47GetHardwareOpusMultiStreamDecoderWorkBufferSizeEiiii" => ms_work,
    "_ZN2nn5codec47GetHardwareOpusMultiStreamDecoderWorkBufferSizeEiiiii" => ms_work,
    "_ZN2nn5codec40InitializeHardwareOpusMultiStreamDecoderEPNS0_34HardwareOpusMultiStreamDecoderTypeEiiiiPKhPvmi" => ms_initialize,
    "_ZN2nn5codec38FinalizeHardwareOpusMultiStreamDecoderEPNS0_34HardwareOpusMultiStreamDecoderTypeE" => ms_finalize,
    "_ZN2nn5codec43GetHardwareOpusMultiStreamDecoderSampleRateEPKNS0_34HardwareOpusMultiStreamDecoderTypeE" => ms_sample_rate,
    "_ZN2nn5codec45GetHardwareOpusMultiStreamDecoderChannelCountEPKNS0_34HardwareOpusMultiStreamDecoderTypeE" => ms_channel_count,
    "_ZN2nn5codec49GetHardwareOpusMultiStreamDecoderTotalStreamCountEPKNS0_34HardwareOpusMultiStreamDecoderTypeE" => ms_total_streams,
    "_ZN2nn5codec50GetHardwareOpusMultiStreamDecoderStereoStreamCountEPKNS0_34HardwareOpusMultiStreamDecoderTypeE" => ms_stereo_streams,
    "_ZN2nn5codec35ResetHardwareOpusMultiStreamDecoderEPNS0_34HardwareOpusMultiStreamDecoderTypeE" => ms_reset_decoder,
    "_ZN2nn5codec44DecodeOpusMultiStreamInterleavedWithHardwareEPNS0_34HardwareOpusMultiStreamDecoderTypeEPmPiPsmPKvm" => ms_decode_frame,
    "_ZN2nn5codec47GetHardwareOpusMultiStreamDecoderProcessingTimeEPKNS0_34HardwareOpusMultiStreamDecoderTypeE" => super::zero64,
}
