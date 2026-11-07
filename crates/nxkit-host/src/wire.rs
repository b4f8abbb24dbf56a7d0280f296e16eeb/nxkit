//! The adapters between the service crates and the kernel: the system events, the guest threads that the services
//! start, and the hooks of the system service. `seams.rs` hands them to the crates.
//!
//! A seam that has no proper implementation yet stops the game with its name (R15).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use nxkit_hos::{GuestThread, Kernel};
use nxkit_svc_system::TextRequest;

use crate::{guestsym, stop};

/// The priority of the threads that a service starts when its seam gives none: the default priority of a Switch thread.
const SERVICE_THREAD_PRIORITY: i32 = 44;
/// The stack of such a thread.
const SERVICE_THREAD_STACK: u64 = 0x10_0000;
/// The result code of a pacing bind that the kernel refused.
const BIND_REFUSED: u32 = 0x1;
/// The number of finished service threads that wait for a join before the next start joins them.
const REAP_AT: usize = 8;

fn kernel() -> Option<&'static Arc<Kernel>> {
    nxkit_rt::kernel()
}

fn need_kernel(what: &str) -> &'static Arc<Kernel> {
    match kernel() {
        Some(k) => k,
        None => stop::fatal(&format!("{what} was called before the kernel started")),
    }
}

/// Binds the `SystemEventType` of the guest at `event` to a new kernel event. It runs SDK code, so the caller is a
/// guest thread.
pub(crate) fn bind_system_event(event: u64, auto_clear: bool) {
    if let Err(why) = need_kernel("bind_system_event").bind_system_event(event, auto_clear) {
        stop::fatal(&format!("binding the system event at {event:#x}: {why}"));
    }
}

/// Signals the event that [`bind_system_event`] made for `event`.
pub(crate) fn signal_system_event(event: u64) {
    if let Err(why) = need_kernel("signal_system_event").signal_system_event(event) {
        eprintln!("nxkit-host: signalling the system event at {event:#x}: {why}");
    }
}

fn clear_system_event(event: u64) {
    if let Err(why) = need_kernel("clear_system_event").clear_system_event(event) {
        eprintln!("nxkit-host: clearing the system event at {event:#x}: {why}");
    }
}

fn dup_readable_handle(handle: u32) -> u32 {
    need_kernel("dup_readable_handle").duplicate_readable_handle(handle).unwrap_or(0)
}

fn fatal(message: &str) {
    stop::fatal(message)
}

/// The bind of the pacing service: the event of the game's `SystemEventType`, and the handle of its writable end.
pub(crate) fn pacing_bind(event: u64) -> Result<u32, u32> {
    let k = kernel().ok_or(BIND_REFUSED)?;
    k.bind_system_event(event, false).map_err(|_| BIND_REFUSED)?;
    Ok(k.system_event_writable_handle(event))
}

/// The guest threads that the services started and that nobody joined yet.
struct Threads {
    next: u64,
    live: Vec<(u64, GuestThread, Arc<AtomicBool>)>,
}

static THREADS: Mutex<Threads> = Mutex::new(Threads { next: 1, live: Vec::new() });

fn threads() -> std::sync::MutexGuard<'static, Threads> {
    THREADS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn(body: impl FnOnce() + Send + 'static, priority: i32, stack: u64) -> Option<(u64, Arc<AtomicBool>)> {
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let thread = GuestThread::spawn(
        move || {
            body();
            flag.store(true, Ordering::Release);
        },
        priority,
        stack,
    )
    .map_err(|why| eprintln!("nxkit-host: starting a service thread: {why}"))
    .ok()?;
    let mut t = threads();
    let id = t.next;
    t.next += 1;
    t.live.push((id, thread, done.clone()));
    Some((id, done))
}

/// Joins the threads that ended.
fn reap() {
    let ended: Vec<GuestThread> = {
        let mut t = threads();
        if t.live.len() < REAP_AT {
            return;
        }
        let (ended, live): (Vec<_>, Vec<_>) = std::mem::take(&mut t.live)
            .into_iter()
            .partition(|(_, _, done)| done.load(Ordering::Acquire));
        t.live = live;
        ended.into_iter().map(|(_, thread, _)| thread).collect()
    };
    for thread in ended {
        if let Err(why) = thread.join() {
            eprintln!("nxkit-host: joining a service thread: {why}");
        }
    }
}

fn audio_spawn(entry: extern "C" fn(u64), arg: u64, priority: i32, stack: usize) -> u64 {
    match spawn(move || entry(arg), priority, stack as u64) {
        Some((id, _)) => id,
        None => 0,
    }
}

fn audio_join(id: u64) {
    let thread = {
        let mut t = threads();
        t.live.iter().position(|(i, _, _)| *i == id).map(|k| t.live.swap_remove(k).1)
    };
    if let Some(thread) = thread {
        if let Err(why) = thread.join() {
            eprintln!("nxkit-host: joining an audio thread: {why}");
        }
    }
}

/// The audio service's view of the host.
pub(crate) fn audio_host() -> nxkit_svc_audio::Host {
    nxkit_svc_audio::Host {
        guest_sym: guestsym::audio_lookup,
        bind_system_event,
        signal_system_event,
        dup_readable_handle,
        spawn_guest_thread: audio_spawn,
        join_guest_thread: audio_join,
        fatal,
    }
}

/// The video service's view of the host.
pub(crate) struct VideoHost;

impl nxkit_svc_video::Host for VideoHost {
    fn resolve(&self, symbol: &str) -> Option<usize> {
        guestsym::video_lookup(symbol)
    }

    fn run_on_guest_thread(&self, delay: Duration, job: Box<dyn FnOnce() + Send>) {
        reap();
        let body = move || {
            std::thread::sleep(delay);
            job();
        };
        if spawn(body, SERVICE_THREAD_PRIORITY, SERVICE_THREAD_STACK).is_none() {
            stop::fatal("video seam run_on_guest_thread could not start a guest thread");
        }
    }

    fn bind_system_event(&self, event: u64) {
        bind_system_event(event, false);
    }

    fn signal_system_event(&self, event: u64) {
        signal_system_event(event);
    }

    fn clear_system_event(&self, event: u64) {
        clear_system_event(event);
    }
}

static HANDHELD: AtomicBool = AtomicBool::new(false);

/// Tells the hooks whether the console is in handheld mode.
pub(crate) fn set_handheld(on: bool) {
    HANDHELD.store(on, Ordering::Relaxed);
}

/// The npad id of player 1: the handheld npad in handheld mode, else the first full key npad.
fn connected_npad() -> u32 {
    if HANDHELD.load(Ordering::Relaxed) { 0x20 } else { 0 }
}

fn text_input(_: &TextRequest) -> Option<String> {
    stop::fatal("system seam text_input is not wired (the software keyboard has no host window yet)")
}

fn nro_load(_: u64, _: u32) -> Option<u64> {
    stop::fatal("system seam nro_load is not wired (the host loads no NRO)")
}

fn nro_unload(_: u64) {
    stop::fatal("system seam nro_unload is not wired (the host loads no NRO)")
}

fn nro_symbol(_: u64, _: &str) -> Option<u64> {
    stop::fatal("system seam nro_symbol is not wired (the host loads no NRO)")
}

fn quit() {
    stop::request("the game quit");
}

/// The hooks of the system service.
pub(crate) fn system_seams() -> nxkit_svc_system::Seams {
    nxkit_svc_system::Seams {
        guest_sym: Some(guestsym::system_lookup),
        hid_connected_npad: Some(connected_npad),
        text_input: Some(text_input),
        native_image_blank: Some(nxkit_svc_video::native_image_blank),
        native_image_free: Some(nxkit_svc_video::native_image_blank_free),
        guest_ns: None,
        execute_program: None,
        nro_load: Some(nro_load),
        nro_unload: Some(nro_unload),
        nro_symbol: Some(nro_symbol),
        quit: Some(quit),
    }
}

/// The hooks with which the input service binds and signals its events.
pub(crate) fn input_hooks() -> nxkit_svc_input::SystemEventHooks {
    nxkit_svc_input::SystemEventHooks { bind: bind_system_event, signal: signal_system_event }
}
