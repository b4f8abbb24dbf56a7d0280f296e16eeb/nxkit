//! The pause state of one thread (`SetThreadActivity`) and the registers that `GetThreadContext3` reports.
//!
//! The state word has three bits. The thread sets `IN_GATE` when a svc starts and clears it when the svc ends, with
//! the address of the gate's register frame in `frame` while the bit is set. Another thread sets `REQUESTED` to pause
//! it. A thread in a svc never clears `IN_GATE` while `REQUESTED` is set: it waits on the word at the end of the svc,
//! so its frame stays valid and it runs no guest code until it is resumed. `READING` is set while another thread
//! reads the frame. A thread that exits does not wait for `REQUESTED`, only for `READING`, because the host frees the
//! gate stack after the exit. The thread itself takes no lock here, so a pause never waits for a lock the thread
//! holds.

use crate::sys;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// The thread is in a svc and its registers are in the gate frame.
pub(crate) const IN_GATE: u32 = 1;
/// Another thread paused the thread.
pub(crate) const REQUESTED: u32 = 2;
/// Another thread reads the gate frame.
pub(crate) const READING: u32 = 4;

/// The size of `ThreadContext` in guest memory.
pub const THREAD_CONTEXT_SIZE: u64 = 0x320;

/// The registers of a thread as `GetThreadContext3` writes them: `svc::ThreadContext` of Horizon for a 64-bit
/// process, 0x320 bytes. The layout follows Atmosphere's `svc_types_common.hpp` and libnx's `ThreadContext`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadContext {
    /// x0 to x28.
    pub x: [u64; 29],
    /// x29.
    pub fp: u64,
    /// x30.
    pub lr: u64,
    /// The stack pointer.
    pub sp: u64,
    /// The program counter.
    pub pc: u64,
    /// The condition flags NZCV in bits 28 to 31.
    pub pstate: u32,
    /// q0 to q31.
    pub v: [u128; 32],
    /// The floating-point control register.
    pub fpcr: u32,
    /// The floating-point status register.
    pub fpsr: u32,
    /// `tpidr_el0`, the TLS block.
    pub tpidr: u64,
}

impl ThreadContext {
    /// The context as the 100 words of the Horizon layout: x0..x28, fp, lr, sp, pc at words 0 to 32, pstate and its
    /// padding at word 33, the vector registers from word 34, fpcr and fpsr at word 98 and tpidr at word 99.
    pub fn to_words(&self) -> [u64; 100] {
        let mut w = [0u64; 100];
        w[..29].copy_from_slice(&self.x);
        (w[29], w[30], w[31], w[32]) = (self.fp, self.lr, self.sp, self.pc);
        w[33] = u64::from(self.pstate);
        for (i, v) in self.v.iter().enumerate() {
            (w[34 + 2 * i], w[35 + 2 * i]) = (*v as u64, (*v >> 64) as u64);
        }
        w[98] = u64::from(self.fpcr) | u64::from(self.fpsr) << 32;
        w[99] = self.tpidr;
        w
    }

    /// The context from the 100 words of [`ThreadContext::to_words`].
    pub fn from_words(w: &[u64; 100]) -> ThreadContext {
        let mut c = ThreadContext::default();
        c.x.copy_from_slice(&w[..29]);
        (c.fp, c.lr, c.sp, c.pc) = (w[29], w[30], w[31], w[32]);
        c.pstate = w[33] as u32;
        for (i, v) in c.v.iter_mut().enumerate() {
            *v = u128::from(w[34 + 2 * i]) | u128::from(w[35 + 2 * i]) << 64;
        }
        (c.fpcr, c.fpsr) = (w[98] as u32, (w[98] >> 32) as u32);
        c.tpidr = w[99];
        c
    }

    pub(crate) fn from_host(r: &sys::HostRegs) -> ThreadContext {
        ThreadContext {
            x: r.x,
            fp: r.fp,
            lr: r.lr,
            sp: r.sp,
            pc: r.pc,
            pstate: r.cpsr,
            v: r.v,
            fpcr: r.fpcr,
            fpsr: r.fpsr,
            tpidr: 0,
        }
    }
}

/// What the kernel needs from the host to pause a thread and read its registers. The host implements it, because it
/// owns the svc gate and knows where the guest's code is.
pub trait ThreadControl: Send + Sync {
    /// The guest registers that the svc gate saved in `frame`, the value the host passed to
    /// [`Kernel::gate_enter`](crate::Kernel::gate_enter). The frame belongs to a thread that waits in the svc, so it does
    /// not change while it is read. x0 to x18 may be left zero, the kernel clears them as Horizon does for a thread in
    /// a svc.
    fn gate_context(&self, frame: u64) -> ThreadContext;

    /// True when `pc` is in the guest's code, where a thread holds no lock of the host. It is called while another
    /// thread is stopped at any point, so it must not allocate or take a lock.
    fn is_guest_code(&self, pc: u64) -> bool;
}

/// How a paused thread is held.
#[derive(Debug)]
pub(crate) enum Held {
    /// Not paused.
    Running,
    /// Paused in a svc. Its registers are in the gate frame.
    Gate,
    /// Stopped in guest code with `thread_suspend`, with the registers it had then.
    Suspended(Box<ThreadContext>),
    /// Paused, but it exited: it reports zero registers.
    Gone,
}

/// The pause state of one thread.
pub(crate) struct Activity {
    /// Held by `SetThreadActivity` and `GetThreadContext3` of other threads, Horizon's activity pause lock. The thread
    /// itself never takes it.
    pub(crate) held: Mutex<Held>,
    pub(crate) word: AtomicU32,
    pub(crate) frame: AtomicU64,
    /// The host thread, set by the thread itself when it starts.
    pub(crate) host: OnceLock<sys::HostThread>,
}

impl Default for Activity {
    fn default() -> Activity {
        Activity {
            held: Mutex::new(Held::Running),
            word: AtomicU32::new(0),
            frame: AtomicU64::new(0),
            host: OnceLock::new(),
        }
    }
}

impl Activity {
    /// The thread enters a svc with its registers in `frame`.
    pub(crate) fn enter_gate(&self, frame: u64) {
        self.frame.store(frame, Ordering::Relaxed);
        self.word.fetch_or(IN_GATE, Ordering::SeqCst);
    }

    /// The thread tries to leave its svc. `None` when it left. Otherwise the word to wait on: the thread is paused
    /// (unless `exiting`) or another thread reads its frame.
    pub(crate) fn try_leave(&self, exiting: bool) -> Option<u32> {
        let mut s = self.word.load(Ordering::SeqCst);
        loop {
            if s & READING != 0 || (!exiting && s & REQUESTED != 0) {
                return Some(s);
            }
            match self
                .word
                .compare_exchange(s, s & !IN_GATE, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return None,
                Err(now) => s = now,
            }
        }
    }

    /// The gate frame, marked as read until `end_read`. `None` when the thread left the gate (it exited).
    pub(crate) fn begin_read(&self) -> Option<u64> {
        let mut s = self.word.load(Ordering::SeqCst);
        loop {
            if s & IN_GATE == 0 {
                return None;
            }
            match self
                .word
                .compare_exchange(s, s | READING, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Some(self.frame.load(Ordering::Relaxed)),
                Err(now) => s = now,
            }
        }
    }

    pub(crate) fn end_read(&self) {
        self.word.fetch_and(!READING, Ordering::SeqCst);
        sys::wake_all(&self.word);
    }

    /// Clears the pause and wakes the thread if it waits at the end of a svc.
    pub(crate) fn clear_request(&self) {
        self.word.fetch_and(!REQUESTED, Ordering::SeqCst);
        sys::wake_all(&self.word);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_follow_the_horizon_layout() {
        let mut c = ThreadContext::default();
        c.x[0] = 1;
        c.x[28] = 28;
        (c.fp, c.lr, c.sp, c.pc, c.pstate) = (29, 30, 31, 32, 0x6000_0000);
        c.v[0] = 0x11 | 0x22 << 64;
        c.v[31] = 0x33 << 64;
        (c.fpcr, c.fpsr, c.tpidr) = (0x44, 0x55, 0x66);
        let w = c.to_words();
        // Byte offsets of Horizon: fp 0xE8, sp 0xF8, pstate 0x108, v 0x110, fpcr 0x310, fpsr 0x314, tpidr 0x318.
        assert_eq!((w[0], w[28], w[0xE8 / 8], w[0xF8 / 8]), (1, 28, 29, 31));
        assert_eq!(w[0x108 / 8], 0x6000_0000);
        assert_eq!((w[0x110 / 8], w[0x110 / 8 + 1]), (0x11, 0x22));
        assert_eq!(w[0x300 / 8 + 1], 0x33);
        assert_eq!(w[0x310 / 8], 0x55 << 32 | 0x44);
        assert_eq!(w[0x318 / 8], 0x66);
        assert_eq!(w.len() as u64 * 8, THREAD_CONTEXT_SIZE);
        assert_eq!(ThreadContext::from_words(&w), c);
    }

    #[test]
    fn a_paused_thread_stays_in_the_gate_and_an_exiting_one_waits_only_for_a_read() {
        let a = Activity::default();
        a.enter_gate(0x1000);
        a.word.fetch_or(REQUESTED, Ordering::SeqCst);
        assert!(a.try_leave(false).is_some());
        assert_eq!(a.begin_read(), Some(0x1000));
        assert!(a.try_leave(true).is_some(), "a read holds the frame");
        a.end_read();
        a.clear_request();
        assert_eq!(a.try_leave(false), None);
        assert_eq!(a.word.load(Ordering::SeqCst), 0);
        assert_eq!(a.begin_read(), None);
    }
}
