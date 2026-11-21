//! Mach thread control for `SetThreadActivity`: a send right to a host thread, `thread_suspend`, `thread_resume` and
//! the registers of a suspended thread (`thread_get_state` with `ARM_THREAD_STATE64` and `ARM_NEON_STATE64`).
//!
//! The right comes from `mach_thread_self` on the thread itself and is held until the thread record drops. After the
//! thread ends the name is a dead name, so a late call fails and never reaches a thread that reused the port.
//!
//! None of these calls allocates or takes a lock in this process, so the kernel may make them while the target is
//! stopped at any point. Other platforms have no thread control: `suspend` returns false.

/// The registers of a suspended host thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostRegs {
    /// x0 to x28.
    pub(crate) x: [u64; 29],
    /// x29.
    pub(crate) fp: u64,
    /// x30.
    pub(crate) lr: u64,
    /// The stack pointer.
    pub(crate) sp: u64,
    /// The program counter.
    pub(crate) pc: u64,
    /// The program status register (NZCV in bits 28 to 31).
    pub(crate) cpsr: u32,
    /// q0 to q31.
    pub(crate) v: [u128; 32],
    /// The floating-point status register.
    pub(crate) fpsr: u32,
    /// The floating-point control register.
    pub(crate) fpcr: u32,
}

#[cfg(target_vendor = "apple")]
mod mach {
    use super::HostRegs;

    pub(super) const KERN_SUCCESS: i32 = 0;
    const ARM_THREAD_STATE64: i32 = 6;
    const ARM_THREAD_STATE64_COUNT: u32 = 68;
    const ARM_NEON_STATE64: i32 = 17;
    const ARM_NEON_STATE64_COUNT: u32 = 132;

    /// `arm_thread_state64_t` without the opaque pointers of arm64e: this is an arm64 process.
    #[repr(C)]
    #[derive(Default)]
    struct ThreadState {
        x: [u64; 29],
        fp: u64,
        lr: u64,
        sp: u64,
        pc: u64,
        cpsr: u32,
        flags: u32,
    }

    /// `arm_neon_state64_t`.
    #[repr(C, align(16))]
    struct NeonState {
        v: [u128; 32],
        fpsr: u32,
        fpcr: u32,
    }

    unsafe extern "C" {
        static mach_task_self_: u32;
        fn mach_thread_self() -> u32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
        fn thread_suspend(thread: u32) -> i32;
        fn thread_resume(thread: u32) -> i32;
        fn thread_get_state(thread: u32, flavor: i32, state: *mut u32, count: *mut u32) -> i32;
    }

    pub(super) fn this_thread() -> u32 {
        // SAFETY: `mach_thread_self` has no preconditions. It adds a user reference that `release` gives back.
        unsafe { mach_thread_self() }
    }

    pub(super) fn release(port: u32) {
        // SAFETY: `port` is a send right of this task from `this_thread`, released once, here.
        unsafe { mach_port_deallocate(mach_task_self_, port) };
    }

    pub(super) fn suspend(port: u32) -> bool {
        // SAFETY: `port` names a thread of this task (or is a dead name, and the call fails).
        unsafe { thread_suspend(port) == KERN_SUCCESS }
    }

    pub(super) fn resume(port: u32) -> bool {
        // SAFETY: as in `suspend`.
        unsafe { thread_resume(port) == KERN_SUCCESS }
    }

    pub(super) fn regs(port: u32) -> Option<HostRegs> {
        let mut gp = ThreadState::default();
        let mut count = ARM_THREAD_STATE64_COUNT;
        // SAFETY: `gp` is an `arm_thread_state64_t` of `count` words, and the call writes at most `count` words.
        let r =
            unsafe { thread_get_state(port, ARM_THREAD_STATE64, (&raw mut gp).cast(), &mut count) };
        if r != KERN_SUCCESS {
            return None;
        }
        let mut fp = NeonState {
            v: [0; 32],
            fpsr: 0,
            fpcr: 0,
        };
        let mut count = ARM_NEON_STATE64_COUNT;
        // SAFETY: `fp` is an `arm_neon_state64_t` of `count` words, aligned to 16 bytes.
        let r =
            unsafe { thread_get_state(port, ARM_NEON_STATE64, (&raw mut fp).cast(), &mut count) };
        if r != KERN_SUCCESS {
            return None;
        }
        Some(HostRegs {
            x: gp.x,
            fp: gp.fp,
            lr: gp.lr,
            sp: gp.sp,
            pc: gp.pc,
            cpsr: gp.cpsr,
            v: fp.v,
            fpsr: fp.fpsr,
            fpcr: fp.fpcr,
        })
    }
}

/// A send right to one host thread.
#[derive(Debug)]
pub(crate) struct HostThread(u32);

impl HostThread {
    /// The calling host thread.
    pub(crate) fn current() -> HostThread {
        #[cfg(target_vendor = "apple")]
        {
            HostThread(mach::this_thread())
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            HostThread(0)
        }
    }

    /// Stops the thread. It returns when the thread runs no more instructions. False when the thread is gone or the
    /// platform has no thread control.
    pub(crate) fn suspend(&self) -> bool {
        #[cfg(target_vendor = "apple")]
        {
            mach::suspend(self.0)
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            false
        }
    }

    /// Lets a thread that `suspend` stopped run again.
    pub(crate) fn resume(&self) -> bool {
        #[cfg(target_vendor = "apple")]
        {
            mach::resume(self.0)
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            false
        }
    }

    /// The registers of the stopped thread.
    pub(crate) fn regs(&self) -> Option<HostRegs> {
        #[cfg(target_vendor = "apple")]
        {
            mach::regs(self.0)
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            None
        }
    }
}

impl Drop for HostThread {
    fn drop(&mut self) {
        #[cfg(target_vendor = "apple")]
        if self.0 != 0 {
            mach::release(self.0);
        }
    }
}
