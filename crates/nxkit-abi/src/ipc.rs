//! Horizon IPC as a service of the nxkit host sees it.
//!
//! `nxkit-ipc` owns the server. It parses the HIPC and CMIF messages in the thread's TLS buffer, keeps the sessions and
//! the domains, counts every command (R16) and stops the game on a command that no service knows (R15). A service crate
//! only implements [`Service`] and gets already parsed requests.
//!
//! A service registers under its Horizon name, for example `fsp-srv`, `hid` or `audren:u`. An interface that a command
//! returns, for example `IFileSystem` from `fsp-srv` command 18, is one more [`Service`] value in
//! [`Response::objects`]. The server turns it into a session handle or a domain object.

use std::sync::Arc;

/// The kind of a buffer descriptor of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferKind {
    /// Type A, a send buffer (`X` and `A` descriptors and `0x5`/`0x21` HIPC auto-select in send direction).
    Send,
    /// Type B, a receive buffer.
    Receive,
    /// Type X, a pointer buffer the kernel copies into the server.
    Pointer,
    /// Type C, a receive list entry.
    ReceiveList,
    /// Type W, an exchange buffer.
    Exchange,
}

/// One buffer of a request. `addr` is a guest address, readable and writable through [`crate::guest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buffer {
    /// The guest address.
    pub addr: u64,
    /// The size in bytes.
    pub size: u64,
    /// The kind of descriptor.
    pub kind: BufferKind,
    /// The index of the descriptor among the descriptors of its kind. CMIF commands name buffers by this index.
    pub index: u32,
}

/// A parsed request for one command of one interface.
#[derive(Debug)]
pub struct Request<'a> {
    /// The CMIF command ID.
    pub command: u32,
    /// The raw data of the command after the CMIF header, aligned as the game wrote it.
    pub data: &'a [u8],
    /// The PID the game sent, if any.
    pub pid: Option<u64>,
    /// Copy handles.
    pub copy_handles: &'a [u32],
    /// Move handles.
    pub move_handles: &'a [u32],
    /// Every buffer of the request, in descriptor order.
    pub buffers: &'a [Buffer],
    /// Domain objects passed in, as interfaces of this server.
    pub objects: &'a [Arc<dyn Service>],
}

impl Request<'_> {
    /// The `n`-th buffer of `kind`.
    pub fn buffer(&self, kind: BufferKind, n: u32) -> Option<Buffer> {
        self.buffers
            .iter()
            .copied()
            .find(|b| b.kind == kind && b.index == n)
    }

    /// A little-endian `u32` at byte `offset` of [`Request::data`].
    pub fn u32_at(&self, offset: usize) -> Option<u32> {
        let b = self.data.get(offset..offset + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A little-endian `u64` at byte `offset` of [`Request::data`].
    pub fn u64_at(&self, offset: usize) -> Option<u64> {
        let b = self.data.get(offset..offset + 8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Some(u64::from_le_bytes(a))
    }
}

/// The answer to one command.
#[derive(Default)]
pub struct Response {
    /// The Horizon result code. Zero is success.
    pub result: u32,
    /// The raw data after the CMIF header.
    pub data: Vec<u8>,
    /// Copy handles to return.
    pub copy_handles: Vec<u32>,
    /// Move handles to return.
    pub move_handles: Vec<u32>,
    /// Interfaces to return. The server makes a session or a domain object of each.
    pub objects: Vec<Arc<dyn Service>>,
}

impl Response {
    /// Appends a little-endian `u32` to [`Response::data`].
    pub fn push_u32(&mut self, v: u32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    /// Appends a little-endian `u64` to [`Response::data`].
    pub fn push_u64(&mut self, v: u64) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    /// Appends raw bytes to [`Response::data`].
    pub fn push_bytes(&mut self, b: &[u8]) {
        self.data.extend_from_slice(b);
    }
}

/// What a service did with a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// The service answered. The response is complete.
    Done,
    /// The service does not know the command. The server stops the game with the interface name and the command ID (R15).
    Unknown,
}

/// One IPC interface. A service crate implements it for every interface it serves.
pub trait Service: Send + Sync {
    /// The name of the interface for logs and counters, for example `fsp-srv` or `IFileSystem`.
    fn name(&self) -> &'static str;

    /// Answers one command. Runs on the guest thread that sent the request.
    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled;
}

impl std::fmt::Debug for dyn Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A service that a crate registers with the server under its Horizon name, for example `fsp-srv`.
#[derive(Clone)]
pub struct Named {
    /// The Horizon service name, at most 8 bytes.
    pub name: &'static str,
    /// The root interface of the service.
    pub root: Arc<dyn Service>,
}
