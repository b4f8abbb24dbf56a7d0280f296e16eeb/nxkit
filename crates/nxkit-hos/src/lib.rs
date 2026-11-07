//! Horizon for the nxkit host: handles, threads, events, locks and waits.
//!
//! Each guest thread is one host thread. There is no global lock. A guest lock, condition variable or address wait
//! keeps its waiters in a sharded table and blocks the thread on its own word with `os_sync_wait_on_address`. The
//! kernel reads guest words through the [`GuestMemory`] trait, so the logic runs without a guest.
//!
//! A thread's QoS class follows its [`ThreadRole`]. The title module gives the table that names the roles. The
//! core rules of Horizon (a thread runs before every lower-priority thread of its core) are off by default and are
//! turned on by [`Config::core_rules`]. Every thread counts its waits, wake-ups and contended locks, and
//! [`Kernel::dump_stats`] prints them.
//!
//! `unsafe` is in the module `sys` only.

mod arbiter;
mod gate;
mod handle;
mod kernel;
mod memory;
mod result;
mod role;
mod stats;
mod svc;
pub mod sys;
mod thread;
mod waitq;

pub use arbiter::{ArbitrationType, SignalType, WAITERS_BIT};
pub use handle::{CURRENT_PROCESS, CURRENT_THREAD};
pub use kernel::{Config, Kernel, MAX_PRIORITY, MAX_WAIT_HANDLES, ThreadSpec, current_thread};
pub use memory::{FakeMemory, GuestMemory};
pub use result::{KResult, KResultOr};
pub use role::{NameMatch, Qos, RoleRule, RoleTable, ThreadInfo, ThreadRole, qos_for_role};
pub use stats::{StatsSnapshot, format_stats};
pub use svc::{GuestLauncher, SvcRegs};
pub use thread::{Thread, ThreadState, Timeout};
