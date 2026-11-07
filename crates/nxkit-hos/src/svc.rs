//! The one table of svc numbers that the kernel answers, with the register convention of the Horizon ABI.
//!
//! The gate of the host calls [`Kernel::dispatch_svc`] with the number and x0 to x7. Each svc is a plain method of
//! the kernel and there is no global lock (R7). A number that this table does not answer returns false, and the
//! host goes on with its own handler.
//!
//! Every svc number that nxnative answers (`dm/host/hle_svc.c` and `dm/host/hle_thread.c`, read on 2026-11-07), and
//! the numbers added here from Horizon:
//!
//! | svc | name | state |
//! |---|---|---|
//! | 0x01 | SetHeapSize | done, `memsvc.rs` |
//! | 0x02 | SetMemoryPermission | done, `memsvc.rs` |
//! | 0x03 | SetMemoryAttribute | done, `memsvc.rs` |
//! | 0x04 | MapMemory | done, `memsvc.rs` |
//! | 0x05 | UnmapMemory | done, `memsvc.rs` |
//! | 0x06 | QueryMemory | done, `memsvc.rs` |
//! | 0x07 | ExitProcess | done per Horizon, not in nxnative |
//! | 0x08 to 0x12 | threads, sleep, priority, core mask, events | done |
//! | 0x13 | MapSharedMemory | done per Horizon, not in nxnative |
//! | 0x14 | UnmapSharedMemory | done per Horizon, not in nxnative |
//! | 0x15 | CreateTransferMemory | done per Horizon, not in nxnative |
//! | 0x16 to 0x1D | handles, waits, locks, condition variables | done |
//! | 0x1E | GetSystemTick | done |
//! | 0x1F | ConnectToNamedPort | done per Horizon, not in nxnative |
//! | 0x20 | SendSyncRequestLight | not answered, returns false |
//! | 0x21 | SendSyncRequest | done per Horizon, not in nxnative |
//! | 0x22 | SendSyncRequestWithUserBuffer | done per Horizon, not in nxnative |
//! | 0x23 | SendAsyncRequestWithUserBuffer | done per Horizon, not in nxnative |
//! | 0x24 | GetProcessId | done per Horizon, not in nxnative |
//! | 0x25 | GetThreadId | done |
//! | 0x26 | Break | done |
//! | 0x27 | OutputDebugString | done |
//! | 0x28 | ReturnFromException | done per Horizon, not in nxnative |
//! | 0x29 | GetInfo | done, `info.rs` |
//! | 0x2A, 0x2B | FlushEntireDataCache, FlushDataCache | done per Horizon, not in nxnative |
//! | 0x2C | MapPhysicalMemory | done, `memsvc.rs` |
//! | 0x2D | UnmapPhysicalMemory | done, `memsvc.rs` |
//! | 0x30, 0x31, 0x37 | GetResourceLimit limit, current and peak value | done per Horizon, not in nxnative |
//! | 0x32 | SetThreadActivity | not answered, returns false. It needs a signal to pause a host thread |
//! | 0x33 | GetThreadContext3 | not answered, returns false. It needs a paused thread |
//! | 0x34, 0x35 | WaitForAddress, SignalToAddress | done |
//! | 0x45 | CreateEvent | done |
//!
//! The memory, IPC and `GetInfo` svcs need the memory model ([`Config::memory`](crate::kernel::Config::memory)).
//! Without it they return false.

use crate::arbiter::{ArbitrationType, SignalType};
use crate::kernel::{Kernel, ThreadSpec};
use crate::result::{KResult, KResultOr};
use crate::thread::{Thread, Timeout};
use std::sync::Arc;

/// The svcs that need the memory model.
const NEEDS_MEMORY: [u32; 18] = [
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x13, 0x14, 0x15, 0x21, 0x22, 0x23, 0x29, 0x2C, 0x2D, 0x30,
    0x31, 0x37,
];

/// The registers x0 to x7 of a svc. The call reads its arguments from them and writes its results back.
pub type SvcRegs = [u64; 8];

/// Makes the host thread body that enters guest code. The host implements it.
pub trait GuestLauncher: Send + Sync {
    /// The body of a guest thread that starts at `entry` with `arg` and the stack top `stack_top`.
    fn body(&self, entry: u64, arg: u64, stack_top: u64) -> Box<dyn FnOnce() + Send>;
}

fn set(regs: &mut SvcRegs, r: KResultOr<()>) {
    regs[0] = u64::from(r.err().unwrap_or(KResult::SUCCESS).raw());
}

/// Writes the result code to x0 and the value to x1.
fn set_value(regs: &mut SvcRegs, r: KResultOr<u64>) {
    match r {
        Ok(v) => (regs[0], regs[1]) = (0, v),
        Err(e) => regs[0] = u64::from(e.raw()),
    }
}

impl Kernel {
    /// Runs the svc `number` for `cur`. Returns false when this crate does not answer that number, and the host
    /// goes on with its own handler. `ExitThread` returns true and the host then ends the host thread.
    pub fn dispatch_svc(&self, cur: &Arc<Thread>, number: u32, regs: &mut SvcRegs) -> bool {
        if self.vm.is_none() && NEEDS_MEMORY.contains(&number) {
            return false;
        }
        let w = |i: usize| regs[i] as u32;
        let ns = regs[3] as i64;
        match number {
            0x01 => {
                let r = self.set_heap_size(regs[1]);
                set_value(regs, r);
            }
            0x02 => set(regs, self.set_memory_permission(regs[0], regs[1], w(2))),
            0x03 => set(
                regs,
                self.set_memory_attribute(regs[0], regs[1], w(2), w(3)),
            ),
            0x04 => set(regs, self.map_memory(regs[0], regs[1], regs[2])),
            0x05 => set(regs, self.unmap_memory(regs[0], regs[1], regs[2])),
            0x06 => {
                let r = self.query_memory(regs[0], regs[2]);
                set_value(regs, r);
            }
            0x07 => self.exit_process(),
            0x08 => {
                let Some(launcher) = self.config.launcher.clone() else {
                    return false;
                };
                let spec = ThreadSpec {
                    name: String::new(),
                    entry: regs[1],
                    priority: w(4) as i32,
                    core: w(5) as i32,
                    stack_size: 0,
                    body: launcher.body(regs[1], regs[2], regs[3]),
                };
                match self.create_thread(spec) {
                    Ok(h) => {
                        regs[0] = 0;
                        regs[1] = u64::from(h);
                    }
                    Err(e) => regs[0] = u64::from(e.raw()),
                }
            }
            0x09 => set(regs, self.start_thread(w(0))),
            0x0A => self.exit_thread(cur),
            0x0B => {
                self.sleep_thread(cur, regs[0] as i64);
                regs[0] = 0;
            }
            0x0C => match self.get_thread_priority(cur, w(1)) {
                Ok(p) => (regs[0], regs[1]) = (0, p as u64),
                Err(e) => regs[0] = u64::from(e.raw()),
            },
            0x0D => set(regs, self.set_thread_priority(cur, w(0), w(1) as i32)),
            0x0E => match self.get_thread_core_mask(cur, w(2)) {
                Ok((ideal, mask)) => {
                    (regs[0], regs[1], regs[2]) = (0, u64::from(ideal as u32), mask)
                }
                Err(e) => regs[0] = u64::from(e.raw()),
            },
            0x0F => set(
                regs,
                self.set_thread_core_mask(cur, w(0), w(1) as i32, regs[2]),
            ),
            0x10 => regs[0] = u64::from(self.current_processor_number(cur)),
            0x11 => set(regs, self.signal_event(w(0))),
            0x12 => set(regs, self.clear_event(w(0))),
            0x16 => set(regs, self.close_handle(w(0))),
            0x17 => set(regs, self.reset_signal(w(0))),
            0x18 => {
                let r = self.wait_sync_from_memory(cur, regs[1], w(2) as usize, ns);
                match r {
                    Ok(i) => (regs[0], regs[1]) = (0, i as u64),
                    Err(e) => (regs[0], regs[1]) = (u64::from(e.raw()), 0),
                }
            }
            0x19 => set(regs, self.cancel_synchronization(w(0))),
            0x1A => set(regs, self.arbitrate_lock(cur, w(0), regs[1], w(2))),
            0x1B => set(regs, self.arbitrate_unlock(regs[0])),
            0x1C => set(
                regs,
                self.wait_process_wide_key_atomic(
                    cur,
                    regs[0],
                    regs[1],
                    w(2),
                    Timeout::from_ns(ns),
                ),
            ),
            0x1D => set(regs, self.signal_process_wide_key(regs[0], w(1) as i32)),
            0x13 => set(regs, self.map_shared_memory(w(0), regs[1], regs[2], w(3))),
            0x14 => set(regs, self.unmap_shared_memory(w(0), regs[1], regs[2])),
            0x15 => {
                let r = self
                    .create_transfer_memory(regs[1], regs[2], w(3))
                    .map(u64::from);
                set_value(regs, r);
            }
            0x1E => {
                // The register that carries the tick is unverified. nxnative writes it to x1 with x0 = 0,
                // Horizon gives it in x0. Both are set.
                let tick = self.system_tick();
                (regs[0], regs[1]) = (tick, tick);
            }
            0x1F => {
                let r = self.connect_to_named_port(regs[1]).map(u64::from);
                set_value(regs, r);
            }
            0x21 => set(regs, self.send_sync_request(cur, w(0))),
            0x22 => set(
                regs,
                self.send_sync_request_with_buffer(cur, regs[0], regs[1], w(2)),
            ),
            0x23 => {
                let r = self
                    .send_async_request_with_buffer(cur, regs[1], regs[2], w(3))
                    .map(u64::from);
                set_value(regs, r);
            }
            0x24 => {
                let r = self.get_process_id(w(1));
                set_value(regs, r);
            }
            0x26 => {
                self.svc_break(regs[0], regs[1], regs[2]);
                regs[0] = 0;
            }
            0x27 => {
                self.output_debug_string(regs[0], regs[1]);
                regs[0] = 0;
            }
            0x28 => self.return_from_exception(regs[0]),
            0x29 => {
                let r = self.get_info(regs[1], regs[3]);
                set_value(regs, r);
            }
            0x2A => regs[0] = 0,
            0x2B => set(regs, self.flush_data_cache(regs[0], regs[1])),
            0x2C => set(regs, self.map_physical_memory(regs[0], regs[1])),
            0x2D => set(regs, self.unmap_physical_memory(regs[0], regs[1])),
            0x30 => {
                let r = self.resource_limit_value(w(1), w(2));
                set_value(regs, r);
            }
            0x31 | 0x37 => {
                let r = self.resource_current_value(w(1), w(2));
                set_value(regs, r);
            }
            0x25 => match self.get_thread_id(cur, w(1)) {
                Ok(id) => (regs[0], regs[1]) = (0, id),
                Err(e) => regs[0] = u64::from(e.raw()),
            },
            0x34 => {
                let r = ArbitrationType::from_raw(w(1)).and_then(|ty| {
                    self.wait_for_address(cur, regs[0], ty, regs[2] as i64, Timeout::from_ns(ns))
                });
                set(regs, r);
            }
            0x35 => {
                let r = SignalType::from_raw(w(1))
                    .and_then(|ty| self.signal_to_address(regs[0], ty, w(2) as i32, w(3) as i32));
                set(regs, r);
            }
            0x45 => match self.create_event() {
                Ok((wr, rd)) => (regs[0], regs[1], regs[2]) = (0, u64::from(wr), u64::from(rd)),
                Err(e) => regs[0] = u64::from(e.raw()),
            },
            _ => return false,
        }
        true
    }

    fn wait_sync_from_memory(
        &self,
        cur: &Arc<Thread>,
        addr: u64,
        count: usize,
        ns: i64,
    ) -> KResultOr<usize> {
        if count > crate::kernel::MAX_WAIT_HANDLES {
            return Err(KResult::OUT_OF_RANGE);
        }
        let mut handles = Vec::with_capacity(count);
        for i in 0..count as u64 {
            handles.push(
                self.mem
                    .load32(addr + 4 * i)
                    .ok_or(KResult::INVALID_CURRENT_MEMORY)?,
            );
        }
        self.wait_synchronization(cur, &handles, Timeout::from_ns(ns))
    }
}
