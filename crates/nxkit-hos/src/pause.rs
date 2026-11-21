//! `SetThreadActivity` (0x32) and `GetThreadContext3` (0x33): one thread pauses another and reads its registers, as a
//! garbage collector does to stop the world (Unity's Boehm GC through `nn::os::SuspendThread` and
//! `nn::os::GetThreadContext`).
//!
//! A pause must never stop a thread while it holds a lock of the host (a kernel table, a service, the allocator),
//! or the pausing thread stops at its next svc. So a thread is paused only where it holds none:
//!
//! - In a svc. The gate saved its registers, and the svc runs on. At the end of the svc the thread waits until it is
//!   resumed (`gate_leave`). A thread that blocks in a svc is paused at once.
//! - In guest code. The kernel stops the host thread with `thread_suspend` and keeps its registers. A thread that is
//!   in host code outside a svc (an SDK take-over that the guest called with a plain branch, the start of a thread) is
//!   let run again for a moment, and the kernel tries again until it is in guest code or in a svc. When it gets to
//!   neither within [`Config::pause_budget`](crate::Config::pause_budget), the process stops with its place.
//!
//! The results follow Mesosphere (`svc_thread.cpp`, `KThread::SetActivity`, `KThread::GetThreadContext3`): an
//! unknown activity is `InvalidEnumValue`, a handle that is not a thread `InvalidHandle`, the calling thread `Busy`, a
//! thread that is not started or has exited `InvalidState`, a second pause or a resume of a thread that is not paused
//! `InvalidState`, and the context of a thread that is not paused `InvalidState`. The pause returns when the thread
//! runs no more guest code. The context of a thread in a svc has x0 to x18 zero, as Horizon reports it.

use crate::kernel::Kernel;
use crate::result::{KResult, KResultOr};
use crate::sync::lock;
use crate::sys;
use crate::thread::{
    Held, IN_GATE, REQUESTED, THREAD_CONTEXT_SIZE, Thread, ThreadContext, ThreadControl,
    ThreadState,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// `ThreadActivity_Runnable`.
const RUNNABLE: u32 = 0;
/// `ThreadActivity_Paused`.
const PAUSED: u32 = 1;
/// The bits of `pstate` that user mode reports (`El0Aarch64PsrMask`: the condition flags).
const PSTATE_MASK: u32 = 0xF000_0000;

/// Where the stop of a thread outside a svc ended.
enum Stop {
    /// In guest code, suspended, with its registers.
    Guest(Box<ThreadContext>),
    /// It entered a svc meanwhile and waits there.
    Gate,
    /// It exited.
    Gone,
    /// It stayed in host code. The last pc it had.
    Stuck(u64),
}

impl Kernel {
    /// The svc gate of the host calls it when a svc of `cur` starts. `frame` is where the gate saved the guest's
    /// registers. It is valid until [`Kernel::gate_leave`] returns, and [`ThreadControl::gate_context`] reads it.
    pub fn gate_enter(&self, cur: &Thread, frame: u64) {
        cur.activity.enter_gate(frame);
    }

    /// The svc gate of the host calls it when the svc of `cur` ends, before the thread runs guest code again. A paused
    /// thread waits here until it is resumed. A thread that exited in the svc does not wait.
    pub fn gate_leave(&self, cur: &Thread) {
        let exiting = cur.state() == ThreadState::Exited;
        let mut parked = false;
        while let Some(word) = cur.activity.try_leave(exiting) {
            if !parked && !exiting {
                // A paused thread is not runnable, so the core rules let the other threads of its core run.
                self.gates.leave(cur);
                parked = true;
            }
            sys::wait_u32(&cur.activity.word, word, None);
        }
        if parked {
            self.gates.enter(cur);
        }
    }

    /// `SetThreadActivity`: 0 resumes a paused thread, 1 pauses it.
    pub fn set_thread_activity(
        &self,
        cur: &Arc<Thread>,
        handle: u32,
        activity: u32,
    ) -> KResultOr<()> {
        if activity != RUNNABLE && activity != PAUSED {
            return Err(KResult::INVALID_ENUM_VALUE);
        }
        let t = self.other_thread(cur, handle)?;
        let mut held = lock(&t.activity.held);
        if t.state() != ThreadState::Started {
            if !matches!(*held, Held::Running) {
                *held = Held::Running;
                t.activity.clear_request();
            }
            return Err(KResult::INVALID_STATE);
        }
        if activity == RUNNABLE {
            return match std::mem::replace(&mut *held, Held::Running) {
                Held::Running => Err(KResult::INVALID_STATE),
                Held::Suspended(_) => {
                    t.activity.clear_request();
                    if let Some(host) = t.activity.host.get() {
                        host.resume();
                    }
                    Ok(())
                }
                Held::Gate | Held::Gone => {
                    t.activity.clear_request();
                    Ok(())
                }
            };
        }
        if !matches!(*held, Held::Running) {
            return Err(KResult::INVALID_STATE);
        }
        let Some(control) = self.config.threads.as_deref() else {
            return Err(KResult::NOT_SUPPORTED);
        };
        if t.activity.word.fetch_or(REQUESTED, Ordering::SeqCst) & IN_GATE != 0 {
            *held = Held::Gate;
            return Ok(());
        }
        *held = match stop_outside_svc(&t, control, self.config.pause_budget) {
            Stop::Guest(ctx) => Held::Suspended(ctx),
            Stop::Gate => Held::Gate,
            Stop::Gone => Held::Gone,
            Stop::Stuck(pc) => {
                t.activity.clear_request();
                (self.config.log)(&format!(
                    "fatal: SetThreadActivity cannot pause thread {:#x} ({:?}): it stayed in host code outside a svc \
                     for {:?}, last pc {pc:#x}. A pause there could stop it while it holds a lock of the host.",
                    t.id,
                    t.name(),
                    self.config.pause_budget
                ));
                std::process::abort()
            }
        };
        Ok(())
    }

    /// `GetThreadContext3`: the registers of a paused thread.
    pub fn get_thread_context3(&self, cur: &Arc<Thread>, handle: u32) -> KResultOr<ThreadContext> {
        let t = self.other_thread(cur, handle)?;
        let held = lock(&t.activity.held);
        let mut ctx = match &*held {
            Held::Running => return Err(KResult::INVALID_STATE),
            Held::Gone => return Ok(ThreadContext::default()),
            Held::Suspended(ctx) => **ctx,
            Held::Gate => {
                let Some(control) = self.config.threads.as_deref() else {
                    return Err(KResult::NOT_SUPPORTED);
                };
                let Some(frame) = t.activity.begin_read() else {
                    return Ok(ThreadContext::default());
                };
                let mut ctx = control.gate_context(frame);
                t.activity.end_read();
                ctx.x[..19].fill(0);
                ctx
            }
        };
        ctx.pstate &= PSTATE_MASK;
        ctx.tpidr = t.tls();
        Ok(ctx)
    }

    /// `GetThreadContext3` as the svc: writes the context to `out` in guest memory.
    pub(crate) fn get_thread_context3_to(
        &self,
        cur: &Arc<Thread>,
        out: u64,
        handle: u32,
    ) -> KResultOr<()> {
        let words = self.get_thread_context3(cur, handle)?.to_words();
        if !out.is_multiple_of(4) || out.checked_add(THREAD_CONTEXT_SIZE).is_none() {
            return Err(KResult::INVALID_POINTER);
        }
        for (i, w) in words.iter().enumerate() {
            if !self.mem.store64(out + 8 * i as u64, *w) {
                return Err(KResult::INVALID_POINTER);
            }
        }
        Ok(())
    }

    /// The thread of `handle`, which must not be the calling thread.
    fn other_thread(&self, cur: &Arc<Thread>, handle: u32) -> KResultOr<Arc<Thread>> {
        let t = if handle == crate::handle::CURRENT_THREAD {
            cur.clone()
        } else {
            self.thread_by_handle(handle)?
        };
        if Arc::ptr_eq(&t, cur) {
            return Err(KResult::BUSY);
        }
        Ok(t)
    }
}

/// Stops `t`, which was outside a svc when the pause was requested, in guest code. While the thread is suspended at a
/// place that is not known to be guest code, this code allocates nothing and takes no lock, because the thread may
/// hold one.
fn stop_outside_svc(t: &Thread, control: &dyn ThreadControl, budget: Duration) -> Stop {
    let t0 = Instant::now();
    let mut last_pc = 0;
    let mut tries = 0u32;
    loop {
        if t.state() == ThreadState::Exited {
            return Stop::Gone;
        }
        if let Some(host) = t.activity.host.get().filter(|h| h.suspend()) {
            if t.activity.word.load(Ordering::SeqCst) & IN_GATE != 0 {
                host.resume();
                return Stop::Gate;
            }
            if let Some(regs) = host.regs() {
                if control.is_guest_code(regs.pc) {
                    return Stop::Guest(Box::new(ThreadContext::from_host(&regs)));
                }
                last_pc = regs.pc;
            }
            host.resume();
        }
        if t0.elapsed() >= budget {
            return Stop::Stuck(last_pc);
        }
        tries += 1;
        if tries < 32 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(Duration::from_micros(u64::from(tries.min(1000))));
        }
    }
}
