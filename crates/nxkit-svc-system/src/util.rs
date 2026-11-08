//! Helpers that every service of the crate shares: result codes, buffer access and kernel events.

use nxkit_abi::guest;
use nxkit_abi::ipc::{Buffer, BufferKind, Handled, Request, Response};
use nxkit_abi::kernel::kernel;
use std::sync::{Mutex, MutexGuard};

/// The failure result of nxnative (`RES_FAIL` in `dm/host/hle.h`). It answers every call that needs a network or an
/// online account, and every request that the host cannot serve.
pub(crate) const RES_FAIL: u32 = 0x2A2;

/// `ICommonStateGetter::ReceiveMessage` when no message is queued: `MAKERESULT(128, 3)` of module `am`.
pub(crate) const RES_NO_MESSAGE: u32 = result(128, 3);

/// The result of a command that needs a buffer the request did not carry: the kernel result `InvalidAddress`.
pub(crate) const RES_NO_BUFFER: u32 = result(1, 0x66);

/// A result from a module and a description, as `MAKERESULT` of the SDK.
pub(crate) const fn result(module: u32, description: u32) -> u32 {
    module | (description << 9)
}

/// A lock that a panic of another thread does not poison. The data stays consistent because every guarded update is a
/// plain assignment.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A response that carries only the result `code`.
pub(crate) fn fail(rsp: &mut Response, code: u32) -> Handled {
    rsp.result = code;
    Handled::Done
}

/// The `n`-th input buffer, as a map alias (`Send`) or as a pointer (`Pointer`).
pub(crate) fn in_buf(req: &Request<'_>, n: u32) -> Option<Buffer> {
    req.buffer(BufferKind::Send, n)
        .or_else(|| req.buffer(BufferKind::Pointer, n))
}

/// The `n`-th output buffer, as a map alias (`Receive`) or as a pointer (`ReceiveList`).
pub(crate) fn out_buf(req: &Request<'_>, n: u32) -> Option<Buffer> {
    req.buffer(BufferKind::Receive, n)
        .or_else(|| req.buffer(BufferKind::ReceiveList, n))
}

/// Up to `max` bytes of the `n`-th input buffer.
pub(crate) fn read_in(req: &Request<'_>, n: u32, max: usize) -> Option<Vec<u8>> {
    let b = in_buf(req, n)?;
    let len = usize::try_from(b.size).unwrap_or(usize::MAX).min(max);
    let mut v = vec![0u8; len];
    guest::read(b.addr, &mut v).ok()?;
    Some(v)
}

/// Writes `data` to the `n`-th output buffer, cut to the size of the buffer. Gives the number of bytes written.
pub(crate) fn write_out(req: &Request<'_>, n: u32, data: &[u8]) -> Option<usize> {
    let b = out_buf(req, n)?;
    let len = usize::try_from(b.size)
        .unwrap_or(usize::MAX)
        .min(data.len());
    guest::write(b.addr, &data[..len]).ok()?;
    Some(len)
}

/// Writes a fixed-size output structure to the `n`-th output buffer. Fails the response when there is no buffer.
pub(crate) fn reply_buf(req: &Request<'_>, n: u32, data: &[u8], rsp: &mut Response) -> Handled {
    match write_out(req, n, data) {
        Some(_) => Handled::Done,
        None => fail(rsp, RES_NO_BUFFER),
    }
}

/// Copies `text` into a zero-padded array of `N` bytes and keeps the last byte as a terminator.
pub(crate) fn padded<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let n = text.len().min(N.saturating_sub(1));
    out[..n].copy_from_slice(&text.as_bytes()[..n]);
    out
}

/// A kernel event that the service creates when it is first needed.
///
/// The service keeps the writable handle and sends the readable one to the game as a copy handle.
pub(crate) struct Event {
    handles: Mutex<Option<(u32, u32)>>,
}

impl Event {
    /// An event that is not created yet.
    pub(crate) const fn new() -> Event {
        Event {
            handles: Mutex::new(None),
        }
    }

    fn ensure(&self) -> Option<(u32, u32)> {
        let mut g = lock(&self.handles);
        if g.is_none() {
            *g = kernel()?.create_event().ok();
        }
        *g
    }

    /// The readable handle, or `None` when the kernel is not there.
    pub(crate) fn readable(&self) -> Option<u32> {
        self.ensure().map(|(_, r)| r)
    }

    /// Signals the event.
    pub(crate) fn signal(&self) {
        if let (Some(k), Some((w, _))) = (kernel(), self.ensure()) {
            let _ = k.signal_event(w);
        }
    }

    /// Clears the event. Does nothing for an event that does not exist yet.
    pub(crate) fn clear(&self) {
        let h = *lock(&self.handles);
        if let (Some(k), Some((w, _))) = (kernel(), h) {
            let _ = k.clear_event(w);
        }
    }

    /// Answers a command that returns the event as a copy handle. A signalled event is signalled before the reply.
    pub(crate) fn reply(&self, rsp: &mut Response, signalled: bool) -> Handled {
        match self.readable() {
            Some(r) => {
                if signalled {
                    self.signal();
                }
                rsp.copy_handles.push(r);
                Handled::Done
            }
            None => fail(rsp, RES_FAIL),
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        let h = *lock(&self.handles);
        if let (Some(k), Some((w, _))) = (kernel(), h) {
            let _ = k.close_handle(w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_composed_like_the_sdk() {
        assert_eq!(RES_NO_MESSAGE, 0x680);
        assert_eq!(RES_NO_BUFFER, 0xCC01);
        assert_eq!(result(2, 1), 0x202);
    }

    #[test]
    fn padded_text_keeps_a_terminator() {
        assert_eq!(padded::<4>("abcdef"), *b"abc\0");
        assert_eq!(padded::<4>("a"), *b"a\0\0\0");
    }
}
