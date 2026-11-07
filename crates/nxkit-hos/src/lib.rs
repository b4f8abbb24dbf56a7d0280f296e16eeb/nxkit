//! Horizon for the nxkit host: handles, threads, events, locks, waits and the process memory.
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
//! With [`Config::memory`] the kernel keeps its own model of the host's reservation (heap, stack and alias regions,
//! shared and transfer memory, the thread-local regions) and answers the memory svcs, `GetInfo` and the IPC svcs.
//! The SDK's own `nn::os` runs on this kernel. [`exports`] lists the few SDK and libc functions that nxnative
//! replaces on top of it, and [`set_guest_sym`] is the seam through which they reach the SDK's own functions.
//!
//! `unsafe` is in the module `sys` only.

mod abi_impl;
mod arbiter;
pub mod entropy;
mod events;
mod gate;
mod guestsym;
mod guestthread;
mod handle;
mod info;
mod ipc;
mod kernel;
mod lend;
mod memory;
mod memsvc;
mod os;
mod process;
mod result;
mod role;
mod shared;
mod space;
mod stats;
mod svc;
pub mod sys;
mod sysevent;
mod thread;
mod tls;
mod vm;
mod waitq;

pub use arbiter::{ArbitrationType, SignalType, WAITERS_BIT};
pub use guestsym::{GuestSymFn, guest_sym, set_guest_sym};
pub use guestthread::GuestThread;
pub use handle::{CURRENT_PROCESS, CURRENT_THREAD};
pub use kernel::{
    Config, Kernel, MAX_PRIORITY, MAX_WAIT_HANDLES, MainThread, ThreadSpec, current_thread,
};
pub use memory::{FakeMemory, GuestMemory, HostSpace, PAGE_SIZE};
pub use os::{OsConfig, exports, forced_names, init_os};
pub use result::{KResult, KResultOr};
pub use role::{NameMatch, Qos, RoleRule, RoleTable, ThreadInfo, ThreadRole, qos_for_role};
pub use stats::{StatsSnapshot, format_stats};
pub use svc::{GuestLauncher, SvcRegs};
pub use thread::{Thread, ThreadState, Timeout};
pub use vm::{MemoryConfig, Module};
