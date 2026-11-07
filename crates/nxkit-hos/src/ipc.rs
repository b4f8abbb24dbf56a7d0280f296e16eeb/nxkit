//! `ConnectToNamedPort` and the three requests that send a message to a session.
//!
//! The kernel holds a session as an `Arc<dyn Session>` behind a handle. The message of `SendSyncRequest` is the IPC
//! buffer in the thread's own thread-local region (the first 0x100 bytes of `tpidrro_el0`, see `tls.rs`). The
//! `WithUserBuffer` forms take a buffer of the game. The server reads the request in the buffer and writes the
//! response there. No kernel lock is held while a session runs, because a service calls back into the kernel (it makes
//! events, handles and shared memory while it handles a command).
//!
//! nxnative does not answer these svcs, its services are linked into the game. They are implemented as Horizon
//! defines them (switchbrew `svc` page, checked on 2026-11-07).

use crate::handle::KObject;
use crate::kernel::Kernel;
use crate::memory::PAGE_SIZE;
use crate::result::{KResult, KResultOr};
use crate::thread::Thread;
use crate::vm::IPC_BUFFER_SIZE;
use nxkit_abi::kernel::Session;
use std::sync::Arc;

/// The length of a port name with its end byte.
const PORT_NAME_LEN: u64 = 12;

impl Kernel {
    /// `ConnectToNamedPort`: reads the name at `name_addr` (at most 11 bytes and a zero byte), asks the IPC server
    /// for a session and returns the handle. A name without the end byte is `OutOfRange`. A server that is not
    /// registered is `NotFound`, and the log says so.
    pub(crate) fn connect_to_named_port(&self, name_addr: u64) -> KResultOr<u32> {
        if name_addr == 0 {
            return Err(KResult::INVALID_POINTER);
        }
        let mut name = Vec::with_capacity(PORT_NAME_LEN as usize);
        let mut ended = false;
        for i in 0..PORT_NAME_LEN {
            let mut byte = [0u8; 1];
            if !self.mem.read_bytes(name_addr + i, &mut byte) {
                return Err(KResult::INVALID_CURRENT_MEMORY);
            }
            if byte[0] == 0 {
                ended = true;
                break;
            }
            name.push(byte[0]);
        }
        if !ended {
            return Err(KResult::OUT_OF_RANGE);
        }
        let Some(server) = nxkit_abi::kernel::server() else {
            (self.config.log)(
                "ConnectToNamedPort has no IPC server, the host did not call set_server",
            );
            return Err(KResult::NOT_FOUND);
        };
        let name = String::from_utf8(name).map_err(|_| KResult::NOT_FOUND)?;
        let session = server.connect_port(&name).map_err(KResult)?;
        self.handles.insert(KObject::Session(session))
    }

    fn session(&self, handle: u32) -> KResultOr<Arc<dyn Session>> {
        match self.handles.get(handle) {
            Some(KObject::Session(s)) => Ok(s),
            _ => Err(KResult::INVALID_HANDLE),
        }
    }

    /// `SendSyncRequest`: the message is the IPC buffer of `cur`.
    pub(crate) fn send_sync_request(&self, cur: &Arc<Thread>, handle: u32) -> KResultOr<()> {
        let session = self.session(handle)?;
        let tlr = cur.tlr();
        if tlr == 0 {
            return Err(KResult::NOT_SUPPORTED);
        }
        self.svc_enter(cur);
        match session.request(tlr, IPC_BUFFER_SIZE) {
            0 => Ok(()),
            code => Err(KResult(code)),
        }
    }

    /// Checks the buffer of the `WithUserBuffer` calls: a page-aligned address and a size in whole pages.
    fn user_buffer(addr: u64, size: u64) -> KResultOr<()> {
        if addr == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(KResult::INVALID_ADDRESS);
        }
        if size == 0 || !size.is_multiple_of(PAGE_SIZE) {
            return Err(KResult::INVALID_SIZE);
        }
        Ok(())
    }

    /// `SendSyncRequestWithUserBuffer`.
    pub(crate) fn send_sync_request_with_buffer(
        &self,
        cur: &Arc<Thread>,
        addr: u64,
        size: u64,
        handle: u32,
    ) -> KResultOr<()> {
        Self::user_buffer(addr, size)?;
        let session = self.session(handle)?;
        self.svc_enter(cur);
        match session.request(addr, size) {
            0 => Ok(()),
            code => Err(KResult(code)),
        }
    }

    /// `SendAsyncRequestWithUserBuffer`: the server answers in the call, then the event is signalled before the call
    /// returns. The game waits on the returned read handle and finds the response in its buffer. A request that
    /// fails at the kernel level returns its code and no handle.
    pub(crate) fn send_async_request_with_buffer(
        &self,
        cur: &Arc<Thread>,
        addr: u64,
        size: u64,
        handle: u32,
    ) -> KResultOr<u32> {
        Self::user_buffer(addr, size)?;
        let session = self.session(handle)?;
        let (write, read) = self.create_event()?;
        self.svc_enter(cur);
        let code = session.request(addr, size);
        let signalled = if code == 0 {
            self.signal_event(write)
        } else {
            Err(KResult(code))
        };
        drop(self.handles.remove(write));
        if let Err(e) = signalled {
            drop(self.handles.remove(read));
            return Err(e);
        }
        Ok(read)
    }
}
