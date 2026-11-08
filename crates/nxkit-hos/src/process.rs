//! The process svcs: `ExitProcess`, `GetSystemTick`, `Break`, `OutputDebugString`, `ReturnFromException` and the cache
//! flushes.
//!
//! The kernel ends the process through [`Config::process_exit`](crate::kernel::Config::process_exit). The host sets
//! it to a function that flushes its logs and exits. `Break` follows nxnative (`dm/host/hle_svc.c`, case 0x26): a
//! reason with bit 31 set is a notification and the call returns. Any other reason ends the process with code 42.
//! `ExitProcess` and `ReturnFromException` are not in nxnative. They follow Horizon, which ends the process.

use crate::kernel::Kernel;
use crate::result::{KResult, KResultOr};
use crate::sys;

/// The exit code of a `Break` that is not a notification (the code of nxnative).
const BREAK_EXIT_CODE: u32 = 42;
/// The bit of a `Break` reason that says the guest goes on after the call.
const NOTIFICATION_ONLY: u64 = 0x8000_0000;
/// The most bytes of one `OutputDebugString` that the kernel logs.
const DEBUG_STRING_CAP: u64 = 1024;

impl Kernel {
    /// `ExitProcess`.
    pub(crate) fn exit_process(&self) {
        (self.config.log)("ExitProcess");
        (self.config.process_exit)(0);
    }

    /// `GetSystemTick`: the system counter, 19.2 MHz on the Switch and 31.25 MHz on the Switch 2.
    pub(crate) fn system_tick(&self) -> u64 {
        sys::system_tick()
    }

    /// `Break`: `reason` is `x0`, `info` the address of the result word in `x1` and `size` is `x2`.
    pub(crate) fn svc_break(&self, reason: u64, info: u64, size: u64) {
        let result = if info != 0 && size >= 4 {
            self.mem.load32(info)
        } else {
            None
        };
        (self.config.log)(&format!(
            "Break reason {reason:#x}, result {}",
            result.map_or("unreadable".to_string(), |r| format!("{r:#x}"))
        ));
        if reason & NOTIFICATION_ONLY == 0 {
            (self.config.process_exit)(BREAK_EXIT_CODE);
        }
    }

    /// `OutputDebugString`: the text goes to the log, cut at 1 KiB. Bytes that cannot be read end the text.
    pub(crate) fn output_debug_string(&self, addr: u64, len: u64) {
        let len = len.min(DEBUG_STRING_CAP) as usize;
        let mut bytes = vec![0u8; len];
        if !self.mem.read_bytes(addr, &mut bytes) {
            (self.config.log)("OutputDebugString points to memory that cannot be read");
            return;
        }
        (self.config.log)(&format!(
            "guest: {}",
            String::from_utf8_lossy(&bytes).trim_end()
        ));
    }

    /// `ReturnFromException`: the process ends, as on Horizon.
    pub(crate) fn return_from_exception(&self, code: u64) {
        (self.config.log)(&format!("ReturnFromException with result {code:#x}"));
        (self.config.process_exit)(BREAK_EXIT_CODE);
    }

    /// `FlushDataCache`: the host's cache is coherent for the guest's data, so the call checks the range and does
    /// nothing. Unverified: whether a game needs more here.
    pub(crate) fn flush_data_cache(&self, addr: u64, size: u64) -> KResultOr<()> {
        if addr.checked_add(size).is_none() {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        Ok(())
    }
}
