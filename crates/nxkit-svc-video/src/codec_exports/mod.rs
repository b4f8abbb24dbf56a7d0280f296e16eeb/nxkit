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

fn warn(what: &str) {
    eprintln!("nxkit-video: warning: {what}");
}

fn red_once(flag: &AtomicBool, what: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        warn(what);
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
    for (chunk, w) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
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

mod decoders;
mod images;
mod sync;
#[cfg(test)]
mod tests;

use decoders::*;
use images::*;
use sync::*;

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
        work_buffer as *const () as usize,
    );
    add(
        format!("Initialize{tag}Decoder"),
        format!("E{s}Pvmmttj{INPUT}{OUTPUT}{csd}"),
        init_full::<C> as *const () as usize,
    );
    add(
        format!("Initialize{tag}Decoder"),
        format!("E{s}mttj{INPUT}{OUTPUT}{csd}"),
        init_remote::<C> as *const () as usize,
    );
    add(
        format!("Finalize{tag}Decoder"),
        format!("E{s}"),
        finalize as *const () as usize,
    );
    add(
        format!("Register{tag}DecoderBuffer"),
        format!("E{s}jPNS0_25VideoDecoderImagePoolTypeEj"),
        register as *const () as usize,
    );
    add(
        format!("Unregister{tag}DecoderBuffer"),
        format!("E{s}j"),
        unregister as *const () as usize,
    );
    add(
        format!("Start{tag}Decoder"),
        format!("E{s}"),
        start as *const () as usize,
    );
    add(
        format!("Stop{tag}Decoder"),
        format!("E{s}"),
        reset as *const () as usize,
    );
    add(
        format!("Flush{tag}Decoder"),
        format!("E{s}"),
        reset as *const () as usize,
    );
    add(
        format!("Get{tag}DecoderSubmissionReadyEvent"),
        format!("EPPNS_2os15SystemEventTypeE{s}"),
        ready_event_out as *const () as usize,
    );
    add(
        format!("Submit{tag}DecoderInputBuffer"),
        format!("EPm{s}PKvmNS_8TimeSpanEb"),
        submit as *const () as usize,
    );
    add(
        format!("SubmitEncrypted{tag}DecoderInputBuffer"),
        format!("EPm{s}PKNS0_18EncryptionPropertyEPKvmNS_8TimeSpanEb"),
        submit_encrypted as *const () as usize,
    );
    add(
        format!("Acquire{tag}DecoderOutputBuffer"),
        format!("EPjPNS_2gs16NativeSyncHandleE{s}"),
        acquire as *const () as usize,
    );
    add(
        format!("Release{tag}DecoderOutputBuffer"),
        format!("E{s}jNS_2gs16NativeSyncHandleE"),
        release as *const () as usize,
    );
    add(
        format!("Get{tag}DecoderOutputBufferProperties"),
        format!("EPtS1_S1_S1_PNS_8TimeSpanEPb{s}j"),
        properties as *const () as usize,
    );
    // The forms of SDK 22.
    add(
        format!("Initialize{tag}DecoderByTransferMemory"),
        format!("E{s}Pvmmttj{INPUT}{OUTPUT}{MODE}{csd}"),
        init_transfer::<C> as *const () as usize,
    );
    add(
        format!("Initialize{tag}DecoderByRemoteMemory"),
        format!("E{s}mttj{INPUT}{OUTPUT}{MODE}{csd}"),
        init_remote_mode::<C> as *const () as usize,
    );
    add(
        format!("Get{tag}DecoderSubmissionReadyEvent"),
        format!("E{s}"),
        ready_event as *const () as usize,
    );
    // The forms of SDK 19.
    add(
        format!("Initialize{tag}Decoder"),
        format!("E{s}mmttj{INPUT}{OUTPUT}{csd}"),
        init_remote19::<C> as *const () as usize,
    );
    add(
        format!("Get{tag}DecoderOutputBufferProperties"),
        format!("EPtS1_S1_S1_PNS_5movie10ColorSpaceEPNS_8TimeSpanEPb{s}j"),
        properties19 as *const () as usize,
    );
    if C == H264 {
        add(
            format!("Initialize{tag}Decoder"),
            format!("E{s}Pvmmttj{INPUT}{OUTPUT}{MODE}{csd}"),
            init_transfer::<C> as *const () as usize,
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
        pref as *const () as usize,
    );
    add(
        "MakeDecodeImagePreference",
        "EttNS0_24VideoDecoderOutputFormatENS0_15ImageTilingModeE".to_string(),
        pref as *const () as usize,
    );
    add(
        "CalculateDecoderImagePoolBuffer",
        format!("EPmS1_{pref_ref}j"),
        pool_size as *const () as usize,
    );
    add(
        "InitializeDecoderImagePool",
        format!("EP{pool_type}Pvm{pref_ref}jNS0_9CodecModeE"),
        pool_with_mode as *const () as usize,
    );
    add(
        "InitializeDecoderImagePool",
        format!("EP{pool_type}Pvm{pref_ref}j"),
        pool_no_mode as *const () as usize,
    );
    add(
        "FinalizeDecoderImagePool",
        format!("EP{pool_type}"),
        pool_finalize as *const () as usize,
    );
    add(
        "GetDecoderImagePoolNativeImageHandle",
        format!("EPK{pool_type}j"),
        native_image_handle as *const () as usize,
    );
    add(
        "ReadDecoderOutputImageRgbaHdr",
        format!("EPK{pool_type}jttttPvmm"),
        read_rgba_hdr as *const () as usize,
    );
    add(
        "ReadDecoderOutputImageRgba",
        format!("EPK{pool_type}jttttPvmm"),
        read_rgba as *const () as usize,
    );
    add(
        "ReadDecoderOutputImageNv12",
        format!("EPK{pool_type}jttttPvmmS4_mm"),
        read_nv12 as *const () as usize,
    );
    push(
        &mut all,
        "_ZN2nn5codec3sdk34MakeDecodeImagePreferenceForCompatEttNS0_24VideoDecoderOutputFormatENS0_9CodecModeE".to_string(),
        pref as *const () as usize,
    );
    push(
        &mut all,
        "_ZN2nn5codec5video10InitializeEv".to_string(),
        service_init as *const () as usize,
    );
    push(
        &mut all,
        "_ZN2nn5codec5video8FinalizeEv".to_string(),
        service_init as *const () as usize,
    );
    family::<VP8>("Vp8", &mut all);
    family::<VP9>("Vp9", &mut all);
    family::<H264>("H264", &mut all);
    family::<AV1>("Av1", &mut all);
    let fixed: [(&str, usize); 7] = [
        (
            "_ZN2nn2gs17TryWaitNativeSyncERKNS0_16NativeSyncHandleE",
            try_wait_native_sync as *const () as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk20CreateSyncPointEventEPNS1_18SyncPointEventTypeE",
            spt_create as *const () as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk25InitializeMultiWaitHolderEPNS_2os19MultiWaitHolderTypeEPNS1_18SyncPointEventTypeE",
            spt_holder as *const () as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk21RequestSyncPointEventEPNS1_18SyncPointEventTypeERKNS1_15SyncPointHandleE",
            spt_request as *const () as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk20CancelSyncPointEventEPNS1_18SyncPointEventTypeE",
            spt_cancel as *const () as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk21TryWaitSyncPointEventEPKNS1_18SyncPointEventTypeE",
            spt_try_wait as *const () as usize,
        ),
        (
            "_ZN2nn6syncpt3sdk21DestroySyncPointEventEPNS1_18SyncPointEventTypeE",
            spt_destroy as *const () as usize,
        ),
    ];
    for (name, addr) in fixed {
        all.push(Export::new(name, addr));
    }
    all
}
