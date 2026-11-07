//! The shared contract of the nxkit host.
//!
//! Every service crate (`nxkit-svc-*`), `nxkit-ipc`, `nxkit-rt` and `nxkit-host` use these types. The crate has no
//! logic of its own beyond bounds checks.
//!
//! - [`guest`]: reads and writes guest memory. The game's code runs at fixed addresses in the host process (R18), so a
//!   guest address is a host address. The host reserves one range for all guest memory and names it once with
//!   [`guest::set_range`]. Every access outside that range is a [`guest::Fault`].
//! - [`export`]: one function that a service gives the game in place of an SDK function (R14).
//! - [`ipc`]: the trait of a service that answers Horizon IPC requests, and the request and response types (R14, R15).

pub mod export;
pub mod guest;
pub mod ipc;
