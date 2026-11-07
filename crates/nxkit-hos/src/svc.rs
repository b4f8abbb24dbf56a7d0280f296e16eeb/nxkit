//! The svc numbers of the calls this crate answers, with the register convention of the Horizon ABI.

use crate::arbiter::{ArbitrationType, SignalType};
use crate::kernel::{Kernel, ThreadSpec};
use crate::result::{KResult, KResultOr};
use crate::thread::{Thread, Timeout};
use std::sync::Arc;

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

impl Kernel {
    /// Runs the svc `number` for `cur`. Returns false when this crate does not answer that number, and the host
    /// goes on with its own handler. `ExitThread` returns true and the host then ends the host thread.
    pub fn dispatch_svc(&self, cur: &Arc<Thread>, number: u32, regs: &mut SvcRegs) -> bool {
        let w = |i: usize| regs[i] as u32;
        let ns = regs[3] as i64;
        match number {
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
