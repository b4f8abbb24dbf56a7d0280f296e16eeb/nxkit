//! The IPC server of the nxkit host.
//!
//! The kernel (`nxkit-hos`) calls [`Ipc`] through the traits `Server` and `Session` of `nxkit_abi::kernel`:
//! `connect_port` on `svc ConnectToNamedPort` and `request` on `svc SendSyncRequest`. The server reads the HIPC
//! message at the guest address, parses the CMIF request, finds the interface (the root of a service, a domain
//! object or an object a command returned), calls `Service::handle` of that interface and writes the reply.
//!
//! How the host builds it:
//!
//! ```ignore
//! let mut named = Vec::new();
//! named.extend(nxkit_svc_fs::services());
//! named.extend(nxkit_svc_offline::services());
//! let ipc = nxkit_ipc::Ipc::new(named)?;
//! nxkit_abi::kernel::set_server(ipc.server());
//! // After a game stop: ipc.stop() names the service, the interface and the command ID. ipc.dump() lists the counts.
//! ```
//!
//! Rules of the server:
//! - A `Response::objects` entry becomes an object of the domain in a domain session, and a new session behind a move
//!   handle (`Kernel::add_session`) outside a domain.
//! - `QueryPointerBufferSize` answers 0, so a client sends every auto-select buffer as a map alias (A or B).
//! - Every command is counted per interface and command ID (R16). See [`Ipc::counts`] and [`Ipc::dump`].
//! - A service name or a command that no service knows stops the game (R15). The server records a [`Stop`], logs it and
//!   returns [`STOP_RESULT`] from `Session::request`. The host checks the result and stops the game with the text of
//!   [`Ipc::stop`].
//! - No global lock sits on the request path. A session has its own domain table. The counters use atomics. The only
//!   shared locks are the read lock of the counter map and the lock that records a stop.

mod cmif;
mod counters;
mod hipc;
mod session;
mod sm;

pub use counters::Count;

use nxkit_abi::ipc::{Named, Service};
use nxkit_abi::kernel::{ResultCode, Server, Session};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

/// What `Session::request` and `Server::connect_port` return when the server stopped the game (R15). It is not a
/// Horizon result code. A Horizon result has 22 bits, and this value has bits above them.
pub const STOP_RESULT: ResultCode = 0xFF00_0001;

/// The Horizon name of the service manager port.
pub const SM_PORT: &str = "sm:";

/// The services of a server by their Horizon name.
pub(crate) type Registry = HashMap<&'static str, Arc<dyn Service>>;

/// Why the server stopped the game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    /// The Horizon service name, for example `fsp-srv`, or the name that no service has.
    pub service: String,
    /// The interface that got the command, for example `IFileSystem`.
    pub interface: String,
    /// The CMIF command ID.
    pub command: u32,
    /// What is missing.
    pub reason: String,
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "service {}, interface {}, command {}: {}",
            self.service, self.interface, self.command, self.reason
        )
    }
}

/// The first stop of a server.
#[derive(Default)]
pub(crate) struct StopSlot {
    first: Mutex<Option<Stop>>,
}

impl StopSlot {
    /// Keeps `stop` when it is the first and logs it. A later stop does not replace the first.
    pub(crate) fn record(&self, stop: Stop) {
        let mut slot = self.first.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            eprintln!("nxkit-ipc: the game stops at {stop}");
            *slot = Some(stop);
        }
    }

    fn get(&self) -> Option<Stop> {
        self.first.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The state that every session of a server shares.
pub(crate) struct Inner {
    pub(crate) registry: Arc<Registry>,
    pub(crate) counters: counters::Counters,
    pub(crate) stop: Arc<StopSlot>,
    pub(crate) sm: Arc<dyn Service>,
}

/// Two services have the same name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateService(pub &'static str);

impl std::fmt::Display for DuplicateService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "two service crates register the service name {}", self.0)
    }
}

impl std::error::Error for DuplicateService {}

/// The IPC server. Cloning it gives another handle to the same server.
#[derive(Clone)]
pub struct Ipc {
    inner: Arc<Inner>,
}

impl Ipc {
    /// A server for the services of all service crates, which are the `services()` of each crate.
    ///
    /// Two services with the same name, or a service named `sm:`, are an error (R15): nothing picks one in silence.
    pub fn new(services: Vec<Named>) -> Result<Ipc, DuplicateService> {
        let mut registry = Registry::new();
        for s in services {
            if s.name == SM_PORT || registry.insert(s.name, s.root).is_some() {
                return Err(DuplicateService(s.name));
            }
        }
        let registry = Arc::new(registry);
        let stop = Arc::new(StopSlot::default());
        let sm: Arc<dyn Service> = Arc::new(sm::Sm {
            registry: registry.clone(),
            stop: stop.clone(),
        });
        Ok(Ipc {
            inner: Arc::new(Inner {
                registry,
                counters: counters::Counters::default(),
                stop,
                sm,
            }),
        })
    }

    /// This server as the `Server` the host registers with `nxkit_abi::kernel::set_server`.
    pub fn server(&self) -> Arc<dyn Server> {
        Arc::new(self.clone())
    }

    /// The first stop, if the server stopped the game.
    pub fn stop(&self) -> Option<Stop> {
        self.inner.stop.get()
    }

    /// The name of every registered service, sorted.
    pub fn service_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.inner.registry.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// The count of every command seen so far, sorted by interface and command ID (R16).
    pub fn counts(&self) -> Vec<Count> {
        self.inner.counters.snapshot()
    }

    /// The counts as text, one line for each interface and command ID: `interface<TAB>command<TAB>count`.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for c in self.counts() {
            let _ = writeln!(out, "{}\t{}\t{}", c.interface, c.command, c.count);
        }
        out
    }
}

impl Server for Ipc {
    fn connect_port(&self, name: &str) -> Result<Arc<dyn Session>, ResultCode> {
        if name == SM_PORT {
            return Ok(session::new_session(&self.inner, SM_PORT, self.inner.sm.clone()));
        }
        self.inner.stop.record(Stop {
            service: name.to_string(),
            interface: "ConnectToNamedPort".to_string(),
            command: 0,
            reason: "the port is not served".to_string(),
        });
        Err(STOP_RESULT)
    }

    fn session_for(&self, iface: Arc<dyn Service>) -> Arc<dyn Session> {
        let service = iface.name();
        session::new_session(&self.inner, service, iface)
    }
}

#[cfg(test)]
mod tests;
