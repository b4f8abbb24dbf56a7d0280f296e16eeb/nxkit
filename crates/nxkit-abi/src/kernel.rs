//! The kernel and the IPC server as the services see them.
//!
//! `nxkit-hos` implements [`Kernel`]. `nxkit-ipc` implements [`Server`] and [`Session`]. The host registers both once
//! with [`set_kernel`] and [`set_server`] before the game starts. The kernel calls the server on `ConnectToNamedPort`
//! and `SendSyncRequest`. A service calls the kernel to make events, sessions and shared memory for its responses.

use crate::ipc::Service;
use std::sync::{Arc, OnceLock};

/// A Horizon result code. Zero is success.
pub type ResultCode = u32;

/// One client session as the kernel holds it behind a handle.
pub trait Session: Send + Sync {
    /// `svc SendSyncRequest` or `SendSyncRequestWithUserBuffer`. The message is at the guest address `message` with
    /// `size` bytes (0x100 for the TLS buffer). The server reads the request there and writes the response there.
    fn request(&self, message: u64, size: u64) -> ResultCode;

    /// The handle was closed. The server drops the session state.
    fn close(&self) {}
}

/// The IPC server as the kernel calls it.
pub trait Server: Send + Sync {
    /// `svc ConnectToNamedPort`, for example with `sm:`.
    fn connect_port(&self, name: &str) -> Result<Arc<dyn Session>, ResultCode>;

    /// A new session for `iface`, for an interface that a command returns outside a domain.
    fn session_for(&self, iface: Arc<dyn Service>) -> Arc<dyn Session>;
}

/// The kernel as a service calls it.
pub trait Kernel: Send + Sync {
    /// A new event. Returns `(writable, readable)` handles. A service keeps the writable one and sends the readable one.
    fn create_event(&self) -> Result<(u32, u32), ResultCode>;

    /// Signals the event of a writable handle.
    fn signal_event(&self, handle: u32) -> Result<(), ResultCode>;

    /// Clears the event of a writable or readable handle.
    fn clear_event(&self, handle: u32) -> Result<(), ResultCode>;

    /// A client session handle for `session`.
    fn add_session(&self, session: Arc<dyn Session>) -> Result<u32, ResultCode>;

    /// Shared memory of `size` bytes, for example the HID shared memory. Returns the handle and the guest address of
    /// the memory at which the service writes it. `MapSharedMemory` of the game maps the same pages at the address the
    /// game chooses.
    fn create_shared_memory(&self, size: u64) -> Result<(u32, u64), ResultCode>;

    /// Closes a handle that a service made and did not give to the game.
    fn close_handle(&self, handle: u32) -> Result<(), ResultCode>;
}

static KERNEL: OnceLock<Arc<dyn Kernel>> = OnceLock::new();
static SERVER: OnceLock<Arc<dyn Server>> = OnceLock::new();

/// Registers the kernel. Only the first call wins. Returns false for a later call.
pub fn set_kernel(k: Arc<dyn Kernel>) -> bool {
    KERNEL.set(k).is_ok()
}

/// The kernel, after [`set_kernel`].
pub fn kernel() -> Option<&'static Arc<dyn Kernel>> {
    KERNEL.get()
}

/// Registers the IPC server. Only the first call wins. Returns false for a later call.
pub fn set_server(s: Arc<dyn Server>) -> bool {
    SERVER.set(s).is_ok()
}

/// The IPC server, after [`set_server`].
pub fn server() -> Option<&'static Arc<dyn Server>> {
    SERVER.get()
}
