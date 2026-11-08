//! `nn::web::OfflineWebSession` and `ShowOfflineHtmlPage`: the offline HTML page of a game. Source: nxnative
//! `dm/host/hle_web.c`.
//!
//! nxkit has no web view yet, so this is the stand-in of nxnative for a page that is not shown. A session start
//! queues the empty "ready" message that a game waits for before `Appear`. `Appear` ends the page at once with exit
//! reason 0 and fires the exit event. `WaitForExit` returns the reason and an empty last URL. The synchronous
//! `ShowOfflineHtmlPage` fails, because no page was shown. `TrySendContentMessage` accepts the message and drops it.
//!
//! The state is a small record under a mutex with a condition variable for `WaitForExit`. A web page is a menu event
//! of the game, never a per-frame call. The exit event is a system event of the game, made with the `event` module.

use super::{RES_FAIL, get_bytes, nothing, put_u8, put_u64};
use crate::event::{self, Block};
use crate::util::lock;
use std::sync::{Condvar, Mutex, PoisonError};

const ARG_SIZE: usize = 0x2000;
const URL_SIZE: u64 = 0x1000;

static EXIT_EVENT: Block = Block::new();

#[derive(PartialEq, Eq, Clone, Copy)]
enum Phase {
    Idle,
    Started,
    Shown,
    Ended,
}

struct Session {
    phase: Phase,
    ready_messages: u32,
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    phase: Phase::Idle,
    ready_messages: 0,
});
static CHANGED: Condvar = Condvar::new();

fn open(phase: Phase) -> bool {
    phase == Phase::Started || phase == Phase::Shown
}

/// Ends the page: the exit event fires and the waiters wake.
fn end(s: &mut Session) {
    if !open(s.phase) {
        return;
    }
    s.phase = Phase::Ended;
    event::signal(EXIT_EVENT.addr(0x100));
    CHANGED.notify_all();
}

/// Starts the stand-in page. A page that is open is replaced.
fn start() {
    let ev = EXIT_EVENT.addr(0x100);
    event::bind(ev, false);
    event::clear(ev);
    let mut s = lock(&SESSION);
    s.phase = Phase::Started;
    s.ready_messages = 1;
}

/// Reads the page document path, as `web_tlv` of nxnative does, for the log. The argument is a TLV list: a count at +0
/// and entries of `{u16 type, u16 size, u32 pad, bytes}` from +8.
fn document_path(arg: u64) -> String {
    let bytes = get_bytes(arg, ARG_SIZE);
    if bytes.len() < ARG_SIZE {
        return String::new();
    }
    let n = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
    let mut o = 8;
    for _ in 0..n {
        if o + 8 > ARG_SIZE {
            break;
        }
        let t = u16::from_le_bytes([bytes[o], bytes[o + 1]]);
        let sz = usize::from(u16::from_le_bytes([bytes[o + 2], bytes[o + 3]]));
        if o + 8 + sz > ARG_SIZE {
            break;
        }
        if t == 6 {
            let v = &bytes[o + 8..o + 8 + sz];
            let end = v.iter().position(|b| *b == 0).unwrap_or(sz);
            return String::from_utf8_lossy(&v[..end]).into_owned();
        }
        o += 8 + sz;
    }
    String::new()
}

/// Writes the result of the ended page: the exit reason, the last URL and its size.
fn put_return(rv: u64) {
    if rv == 0 {
        return;
    }
    put_u64(rv, 0);
    put_u8(rv + 8, 0);
    put_u64(rv + 8 + URL_SIZE, 0);
}

/// Waits for the end of the page and resets the session.
fn finish(rv: u64) {
    let mut s = lock(&SESSION);
    while open(s.phase) {
        s = CHANGED.wait(s).unwrap_or_else(PoisonError::into_inner);
    }
    s.phase = Phase::Idle;
    s.ready_messages = 0;
    drop(s);
    put_return(rv);
    event::clear(EXIT_EVENT.addr(0x100));
}

extern "C" fn work_size() -> u64 {
    0xd0
}

extern "C" fn session_finalize(_this: u64) {
    let open_now = open(lock(&SESSION).phase);
    if open_now {
        end(&mut lock(&SESSION));
        finish(0);
    }
}

/// `OfflineWebSession::Start(SystemEventType**, const ShowOfflineHtmlPageArg&)`.
extern "C" fn session_start(_this: u64, event_out: u64, arg: u64) -> u32 {
    super::log(&format!(
        "web: offline page {} (no web view, the stand-in answers)",
        document_path(arg)
    ));
    start();
    put_u64(event_out, EXIT_EVENT.addr(0x100));
    0
}

/// `Appear()`: the stand-in page closes at once.
extern "C" fn session_appear(_this: u64) -> u32 {
    let mut s = lock(&SESSION);
    if open(s.phase) {
        s.phase = Phase::Shown;
        end(&mut s);
    }
    1
}

/// `TrySendContentMessage(const char*, size_t)`: accepted and dropped.
extern "C" fn session_send(_this: u64, _msg: u64, _n: u64) -> u32 {
    1
}

/// `TryReceiveContentMessage(size_t* out, char* buffer, size_t size)`: the empty ready message, once.
extern "C" fn session_receive(_this: u64, out: u64, buffer: u64, size: u64) -> u32 {
    let mut s = lock(&SESSION);
    if s.ready_messages == 0 || !open(s.phase) {
        return 0;
    }
    s.ready_messages -= 1;
    drop(s);
    if buffer != 0 && size != 0 {
        put_u8(buffer, 0);
    }
    put_u64(out, 0);
    1
}

extern "C" fn session_needs_wait(_this: u64) -> u32 {
    u32::from(lock(&SESSION).phase != Phase::Idle)
}

extern "C" fn session_request_exit(_this: u64) {
    end(&mut lock(&SESSION));
}

/// `WaitForExit(OfflineHtmlPageReturnValue*)`.
extern "C" fn session_wait_for_exit(_this: u64, rv: u64) -> u32 {
    finish(rv);
    0
}

/// `ShowOfflineHtmlPage(OfflineHtmlPageReturnValue*, const ShowOfflineHtmlPageArg&)`: fails, no page is shown.
extern "C" fn show_page(_rv: u64, arg: u64) -> u32 {
    super::log(&format!(
        "web::ShowOfflineHtmlPage {}: no page shown",
        document_path(arg)
    ));
    RES_FAIL
}

table! {
    "_ZN2nn3web17OfflineWebSessionC1Ev" => nothing,
    "_ZN2nn3web17OfflineWebSessionD1Ev" => nothing,
    "_ZN2nn3web17OfflineWebSession17GetWorkBufferSizeEv" => work_size,
    "_ZN2nn3web17OfflineWebSession10InitializeEPvm" => nothing,
    "_ZN2nn3web17OfflineWebSession8FinalizeEv" => session_finalize,
    "_ZN2nn3web17OfflineWebSession5StartEPPNS_2os15SystemEventTypeERKNS0_22ShowOfflineHtmlPageArgE" => session_start,
    "_ZN2nn3web17OfflineWebSession6AppearEv" => session_appear,
    "_ZN2nn3web17OfflineWebSession21TrySendContentMessageEPKcm" => session_send,
    "_ZN2nn3web17OfflineWebSession24TryReceiveContentMessageEPmPcm" => session_receive,
    "_ZNK2nn3web17OfflineWebSession16NeedsWaitForExitEv" => session_needs_wait,
    "_ZN2nn3web17OfflineWebSession11RequestExitEv" => session_request_exit,
    "_ZN2nn3web17OfflineWebSession11WaitForExitEPNS0_26OfflineHtmlPageReturnValueE" => session_wait_for_exit,
    "_ZN2nn3web19ShowOfflineHtmlPageEPNS0_26OfflineHtmlPageReturnValueERKNS0_22ShowOfflineHtmlPageArgE" => show_page,
}
