//! `GetInfo` and the resource limit calls.
//!
//! The info types and their values follow nxnative (`dm/host/hle_svc.c`, case 0x29, the enum `I_*`) and the Horizon
//! numbers in switchbrew's `svc` page (checked on 2026-11-07). The handle argument is not checked, as in nxnative.
//! Every type that the SDK asks for at start is answered from the memory model. Type 9 gives a resource limit handle,
//! as Horizon does. Nxnative does not name types 10 and 19, and it answers every unknown number with zero and a log.
//! Here they return `InvalidEnumValue`, and the log names the number (R15).
//!
//! The resource limits are an approximation. `PhysicalMemory` has the application pool as its limit and `Threads` has
//! 96, the limit that nxnative reads for an application from `pm` (Atmosphere `pm_spec.cpp`). The other resources
//! answer `NotSupported`. The peak value is the current value (unverified, nothing in the SDK reads it).

use crate::handle::{CURRENT_PROCESS, KObject};
use crate::kernel::Kernel;
use crate::result::{KResult, KResultOr};
use crate::thread::{ThreadState, lock};
use crate::vm::Vm;

/// The thread limit of an application.
const THREAD_LIMIT: u64 = 96;
/// The system resource that `GetInfo` reports as used when the title has one: 4 MiB.
const SYSTEM_RESOURCE_USED: u64 = 0x40_0000;
/// The page size of the Horizon kernel.
const KERNEL_PAGE_SIZE: u64 = 0x1000;

impl Kernel {
    fn alive_threads(&self) -> u64 {
        lock(&self.threads)
            .iter()
            .filter(|t| t.state() != ThreadState::Exited)
            .count() as u64
    }

    fn used_memory(&self, vm: &Vm) -> u64 {
        let sp = lock(&vm.space);
        let physical = if vm.cfg.map_limit {
            sp.physical_used
        } else {
            0
        };
        vm.cfg.used_memory + sp.heap_size + physical
    }

    /// `GetInfo`: the value of info type `id` with sub id `sub`.
    pub(crate) fn get_info(&self, id: u64, sub: u64) -> KResultOr<u64> {
        let vm = self.vm()?;
        let cfg = &vm.cfg;
        let lay = &vm.layout;
        Ok(match id {
            0 => self.config.core_mask,
            1 => 0,
            2 => lay.alias_base,
            3 => lay.alias_end - lay.alias_base,
            4 => lay.heap_base,
            5 => lay.heap_cap,
            6 | 21 => cfg.app_pool,
            7 | 22 => self.used_memory(vm),
            8 => 0,
            9 => u64::from(self.resource_limit_handle()?),
            11 => {
                if sub > 3 {
                    return Err(KResult::INVALID_COMBINATION);
                }
                crate::entropy::next64()
            }
            12 => lay.base,
            13 => lay.end - lay.base,
            14 => lay.stack_base,
            15 => lay.stack_end - lay.stack_base,
            16 => cfg.system_resource,
            17 if cfg.system_resource != 0 => SYSTEM_RESOURCE_USED,
            17 => 0,
            18 => cfg.program_id,
            20 => vm.exception_context(),
            23 => 1,
            24 => THREAD_LIMIT.saturating_sub(self.alive_threads()),
            25..=32 => 0,
            33 => KERNEL_PAGE_SIZE,
            _ => {
                (self.config.log)(&format!(
                    "GetInfo type {id} (sub id {sub}) is not answered. Horizon has no value for it in this kernel"
                ));
                return Err(KResult::INVALID_ENUM_VALUE);
            }
        })
    }

    /// The handle of the process's resource limit. It is made on the first call.
    fn resource_limit_handle(&self) -> KResultOr<u32> {
        if let Some(h) = self.resource_limit.get() {
            return Ok(*h);
        }
        let h = self.handles.insert(KObject::ResourceLimit)?;
        match self.resource_limit.set(h) {
            Ok(()) => Ok(h),
            Err(_) => {
                // Another thread made the handle first. Ours is not needed.
                drop(self.handles.remove(h));
                self.resource_limit
                    .get()
                    .copied()
                    .ok_or(KResult::INVALID_STATE)
            }
        }
    }

    /// The limit and the current value of a limitable resource. Resource 0 is physical memory, 1 is threads.
    fn resource_values(&self, handle: u32, which: u32) -> KResultOr<(u64, u64)> {
        let vm = self.vm()?;
        if !matches!(self.handles.get(handle), Some(KObject::ResourceLimit)) {
            return Err(KResult::INVALID_HANDLE);
        }
        match which {
            0 => Ok((vm.cfg.app_pool, self.used_memory(vm))),
            1 => Ok((THREAD_LIMIT, self.alive_threads())),
            2..=4 => {
                (self.config.log)(&format!(
                    "resource limit {which} (events, transfer memories or sessions) is not modelled"
                ));
                Err(KResult::NOT_SUPPORTED)
            }
            _ => Err(KResult::INVALID_ENUM_VALUE),
        }
    }

    /// `GetResourceLimitLimitValue`.
    pub(crate) fn resource_limit_value(&self, handle: u32, which: u32) -> KResultOr<u64> {
        self.resource_values(handle, which).map(|(limit, _)| limit)
    }

    /// `GetResourceLimitCurrentValue`, and `GetResourceLimitPeakValue`, which gives the same value.
    pub(crate) fn resource_current_value(&self, handle: u32, which: u32) -> KResultOr<u64> {
        self.resource_values(handle, which)
            .map(|(_, current)| current)
    }

    /// `GetProcessId`: only the handle of the current process is known.
    pub(crate) fn get_process_id(&self, handle: u32) -> KResultOr<u64> {
        if handle == CURRENT_PROCESS {
            Ok(self.config.process_id)
        } else {
            Err(KResult::INVALID_HANDLE)
        }
    }
}
