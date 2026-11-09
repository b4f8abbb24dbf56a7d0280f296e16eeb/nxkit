//! The SDK take-overs of the audio renderer: the work buffer size, open and close. They follow `hle_audren.c` of nxnative.
//!
//! A game asks for the DSP renderer of the console. The host opens the SDK's CPU renderer in manual mode instead, runs it
//! on its own render thread and plays what the DeviceSink produces. The game still sees the work buffer size and the
//! renderer event of the DSP service.

use crate::host;
use crate::ren_run::{h_ars_exec, h_impl_exec, h_update, start_thread};
use crate::ren_scan::IMPL_EXEC;
use crate::ren_state::{Ren, SLOTS, Sym, T_IN_OPEN, by_handle, free_slot, lk};
use crate::ren_work::service_work_size;
use crate::sys::call::call;
use crate::util::{ObjMap, RES_FAIL, args, ex};
use nxkit_abi::export::Export;
use nxkit_abi::guest;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const P_SIZE: usize = 0x40;
const P_DEVICE: usize = 0x2c;
const P_MODE: usize = 0x30;
const PAGE: u64 = 0x1000;

const WORK_SIZE: &str =
    "_ZN2nn5audio30GetAudioRendererWorkBufferSizeERKNS0_22AudioRendererParameterE";
const OPEN_EV: &str = "_ZN2nn5audio17OpenAudioRendererEPNS0_19AudioRendererHandleEPNS_2os11SystemEventERKNS0_22AudioRendererParameterEPvm";
const OPEN: &str =
    "_ZN2nn5audio17OpenAudioRendererEPNS0_19AudioRendererHandleERKNS0_22AudioRendererParameterEPvm";
const CLOSE: &str = "_ZN2nn5audio18CloseAudioRendererENS0_19AudioRendererHandleE";
const UP_ALLOC: &str = "_ZN2nn5audio6server16UpsamplerManager8AllocateEv";
const UPDATE: &str = "_ZN2nn5audio6server17AudioRenderSystem6UpdateEPvmS3_mPKvm";
const ARS_EXEC: &str = "_ZN2nn5audio6server17AudioRenderSystem29ExecuteAudioRendererRenderingEv";

static S_WORK_SIZE: Sym = Sym::new(WORK_SIZE);
static S_OPEN_EV: Sym = Sym::new(OPEN_EV);
static S_OPEN: Sym = Sym::new(OPEN);
static S_CLOSE: Sym = Sym::new(CLOSE);
static S_UP_ALLOC: Sym = Sym::new(UP_ALLOC);
static S_DESTROY: Sym = Sym::new("_ZN2nn2os18DestroySystemEventEPNS0_15SystemEventTypeE");
static S_ATTACH: Sym = Sym::new(
    "_ZN2nn2os33AttachReadableHandleToSystemEventEPNS0_15SystemEventTypeEjbNS0_14EventClearModeE",
);
static S_READABLE: Sym =
    Sym::new("_ZN2nn2os30GetReadableHandleOfSystemEventEPKNS0_15SystemEventTypeE");

/// Work buffers that the host allocated for the CPU renderer, by renderer object. Close frees them.
static OWNED: ObjMap<Arc<Vec<u8>>> = ObjMap::new();
static WORK_LOGGED: AtomicBool = AtomicBool::new(false);

struct RevSlot {
    obj: u64,
    ev: Box<[u128; 8]>,
}

/// Four `SystemEvent` objects of 0x80 bytes that the SDK keeps as the renderer's own event until Close. The game's object
/// may go away at any time.
static REVS: Mutex<[Option<RevSlot>; 4]> = Mutex::new([const { None }; 4]);

fn rev_take() -> Option<u64> {
    let mut slots = lk(&REVS);
    let free = slots.iter_mut().find(|s| s.is_none())?;
    let ev = Box::new([0u128; 8]);
    let addr = ev.as_ptr() as u64;
    *free = Some(RevSlot { obj: 1, ev });
    Some(addr)
}

/// Binds the event at `ev` to the renderer `obj`. An `obj` of 0 frees it.
fn rev_set(ev: u64, obj: u64) {
    let mut slots = lk(&REVS);
    for s in slots.iter_mut() {
        if s.as_ref().is_some_and(|x| x.ev.as_ptr() as u64 == ev) {
            if obj == 0 {
                *s = None;
            } else if let Some(x) = s.as_mut() {
                x.obj = obj;
            }
        }
    }
}

fn rev_find(obj: u64) -> Option<u64> {
    lk(&REVS)
        .iter()
        .flatten()
        .find(|s| s.obj == obj)
        .map(|s| s.ev.as_ptr() as u64)
}

fn read_param(param: u64) -> [u8; P_SIZE] {
    let mut p = [0u8; P_SIZE];
    if guest::read(param, &mut p).is_err() {
        host::fatal(&format!(
            "audren: the renderer parameter at {param:#x} is not readable"
        ));
    }
    p
}

fn i32_at(p: &[u8; P_SIZE], o: usize) -> i32 {
    i32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}

/// The parameter with the CPU renderer in manual mode.
fn cpu_manual(src: &[u8; P_SIZE]) -> [u8; P_SIZE] {
    let mut p = *src;
    p[P_DEVICE..P_DEVICE + 4].copy_from_slice(&1i32.to_le_bytes());
    p[P_MODE..P_MODE + 4].copy_from_slice(&1i32.to_le_bytes());
    p
}

/// `GetAudioRendererWorkBufferSize`: the game sizes its buffer for the DSP service.
extern "C" fn h_work_size(param: u64) -> u64 {
    let src = read_param(param);
    let p = cpu_manual(&src);
    let n = call(S_WORK_SIZE.addr(), args(&[p.as_ptr() as u64])) as usize;
    let service = if i32_at(&src, P_DEVICE) == 0 {
        service_work_size(&p)
    } else {
        0
    };
    let guest_n = if service != 0 { service } else { n };
    if !WORK_LOGGED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "nxkit-audio: audren: work buffer guest {guest_n:#x}, local CPU and manual renderer {n:#x}"
        );
    }
    guest_n as u64
}

/// Allocates `need` zeroed bytes aligned to a page. The vector keeps the memory alive.
fn page_block(need: usize) -> (Arc<Vec<u8>>, u64) {
    let v = Arc::new(vec![0u8; need + PAGE as usize]);
    let base = v.as_ptr() as u64;
    let aligned = (base + PAGE - 1) & !(PAGE - 1);
    (v, aligned)
}

/// Both `OpenAudioRenderer` forms. `ev` is the game's event, or 0 for the form without one.
fn ar_open(handle: u64, ev: u64, param: u64, work: u64, sz: u64) -> u32 {
    // The SDK's event form opens through the form without an event. The nested call goes straight to the SDK.
    if T_IN_OPEN.with(|t| t.get()) {
        return call(S_OPEN.addr(), args(&[handle, param, work, sz])) as u32;
    }
    let src = read_param(param);
    let p = cpu_manual(&src);
    let device = i32_at(&src, P_DEVICE);
    eprintln!(
        "nxkit-audio: audren: OpenAudioRenderer{} rate {} samples per frame {} mix buffers {} submixes {} voices {} sinks {} effects {} splitters {} (original device {device} mode {} to CPU and manual) work buffer {work:#x}+{sz:#x}",
        if ev == 0 { " (no event)" } else { "" },
        i32_at(&src, 0),
        i32_at(&src, 4),
        i32_at(&src, 8),
        i32_at(&src, 0xc),
        i32_at(&src, 0x10),
        i32_at(&src, 0x14),
        i32_at(&src, 0x18),
        i32_at(&src, 0x24),
        i32_at(&src, P_MODE)
    );
    // The game's buffer follows the DSP service contract. The CPU renderer may need more, which the host then supplies.
    let (mut work, mut sz) = (work, sz);
    let mut owned: Option<Arc<Vec<u8>>> = None;
    if device == 0 && work != 0 && work % PAGE == 0 && sz >= PAGE && sz % PAGE == 0 {
        let need = call(S_WORK_SIZE.addr(), args(&[p.as_ptr() as u64])) as usize;
        let service = service_work_size(&p);
        let guest_need = if service != 0 { service } else { need };
        if (sz as usize) < guest_need {
            eprintln!(
                "nxkit-audio: [RED] audren: work buffer {sz:#x} is smaller than the service requirement {guest_need:#x}"
            );
            return RES_FAIL;
        }
        if need > sz as usize {
            let (v, aligned) = page_block(need);
            work = aligned;
            sz = need as u64;
            owned = Some(v);
            eprintln!(
                "nxkit-audio: audren: the host backs the local renderer work buffer {work:#x}+{sz:#x}"
            );
        }
    }
    // The SDK keeps the event it gets until Close. The game's own object may go away, so the SDK gets one of the host.
    let sev = if ev != 0 && device == 0 {
        rev_take()
    } else {
        None
    };
    if ev != 0 && device == 0 && sev.is_none() {
        eprintln!(
            "nxkit-audio: [RED] audren: all renderer events are in use: the SDK keeps the game's event, which must outlive the renderer"
        );
    }
    T_IN_OPEN.with(|t| t.set(true));
    let pp = p.as_ptr() as u64;
    let r = if ev != 0 {
        call(
            S_OPEN_EV.addr(),
            args(&[handle, sev.unwrap_or(ev), pp, work, sz]),
        ) as u32
    } else {
        call(S_OPEN.addr(), args(&[handle, pp, work, sz])) as u32
    };
    T_IN_OPEN.with(|t| t.set(false));
    if r != 0 {
        if let Some(s) = sev {
            rev_set(s, 0);
        }
        eprintln!("nxkit-audio: [RED] audren: the SDK's OpenAudioRenderer failed with {r:#x}");
        return r;
    }
    let h0 = guest::read_u64(handle).unwrap_or(0);
    let h1 = guest::read_u64(handle + 8).unwrap_or(0);
    if let Some(o) = owned {
        OWNED.put(h0, o);
    }
    if ev != 0 && device == 0 {
        // The renderer's event becomes a kernel event, as on the console. The game's object gets a copy of the readable
        // handle and owns it. A signal of a kernel event wakes every waiter.
        match sev {
            Some(s) => {
                call(S_DESTROY.addr(), args(&[s]));
                host::bind_event(s, true);
                let readable = call(S_READABLE.addr(), args(&[s])) as u32;
                let copy = host::dup_readable(readable);
                call(S_ATTACH.addr(), args(&[ev, u64::from(copy), 1, 1]));
                rev_set(s, h0);
            }
            None => {
                call(S_DESTROY.addr(), args(&[ev]));
                host::bind_event(ev, true);
            }
        }
    }
    let ren = {
        let mut slots = lk(&SLOTS);
        match slots.iter().position(|s| s.is_none()) {
            Some(i) => {
                let r = Arc::new(Ren::new(i, h0, h1));
                slots[i] = Some(r.clone());
                Some(r)
            }
            None => None,
        }
    };
    let Some(ren) = ren else {
        eprintln!(
            "nxkit-audio: [RED] audren: more than {} renderers: this one is not rendered",
            crate::ren_state::AR_MAX
        );
        return r;
    };
    eprintln!("nxkit-audio: audren: renderer {} opened", ren.idx);
    start_thread(&ren);
    r
}

extern "C" fn h_open(handle: u64, ev: u64, param: u64, work: u64, sz: u64) -> u32 {
    ar_open(handle, ev, param, work, sz)
}

extern "C" fn h_open0(handle: u64, param: u64, work: u64, sz: u64) -> u32 {
    ar_open(handle, 0, param, work, sz)
}

/// `CloseAudioRenderer`: stops the render thread first, then hands over to the SDK. The render thread may be inside the
/// SDK's render, which uses the renderer that Close is about to free.
extern "C" fn h_close(obj: u64, state: u64) {
    let a = by_handle(obj);
    if let Some(a) = &a
        && a.tt.load(Ordering::Acquire) != 0
    {
        a.open.store(false, Ordering::Release);
        host::join_thread(a.tt.swap(0, Ordering::AcqRel));
    }
    let owned = OWNED.del(obj);
    let sev = rev_find(obj);
    call(S_CLOSE.addr(), args(&[obj, state]));
    drop(owned);
    if let Some(a) = &a {
        free_slot(a);
    }
    if let Some(s) = sev {
        // The SDK's Close does not touch the event. Destroying it closes the renderer's ends and leaves the game's copy.
        call(S_DESTROY.addr(), args(&[s]));
        rev_set(s, 0);
    }
}

/// `UpsamplerManager::Allocate`: a DeviceSink below 48 kHz gets no upsampler with CPU rendering, the host resamples.
extern "C" fn h_up_alloc(mgr: u64) -> u64 {
    if mgr == 0 {
        0
    } else {
        call(S_UP_ALLOC.addr(), args(&[mgr]))
    }
}

/// Adds the renderer take-overs to `v`.
pub(crate) fn exports(v: &mut Vec<Export>) {
    ex!(v, WORK_SIZE, h_work_size);
    ex!(v, OPEN_EV, h_open);
    ex!(v, OPEN, h_open0);
    ex!(v, CLOSE, h_close);
    ex!(v, UP_ALLOC, h_up_alloc);
    ex!(v, IMPL_EXEC, h_impl_exec);
    ex!(v, ARS_EXEC, h_ars_exec);
    ex!(v, UPDATE, h_update);
}
