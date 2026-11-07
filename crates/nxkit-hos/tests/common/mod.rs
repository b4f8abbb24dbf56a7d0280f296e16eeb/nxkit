//! Helpers shared by the test files: a kernel over fake memory, host threads that act as guest threads, and a
//! guest mutex and condition variable written as the SDK writes them, over the svc calls.

#![allow(dead_code, unreachable_pub)]

use nxkit_hos::{
    Config, FakeMemory, GuestMemory, KResult, Kernel, Thread, ThreadSpec, Timeout, WAITERS_BIT,
    current_thread,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const BASE: u64 = 0x10_0000;

pub struct World {
    pub kernel: Arc<Kernel>,
    pub mem: Arc<FakeMemory>,
    pub main: Arc<Thread>,
}

pub fn world() -> World {
    world_with(Config::default())
}

pub fn world_with(config: Config) -> World {
    let mem = Arc::new(FakeMemory::new(BASE, 256));
    let kernel = Kernel::new(config, mem.clone());
    let main = kernel
        .attach_current_thread("main", 30, 0)
        .expect("attach the test thread");
    World { kernel, mem, main }
}

impl World {
    pub fn addr(&self, word: usize) -> u64 {
        self.mem.addr(word)
    }

    pub fn load(&self, word: usize) -> u32 {
        self.mem.load32(self.addr(word)).unwrap_or(u32::MAX)
    }

    pub fn store(&self, word: usize, v: u32) {
        assert!(self.mem.store32(self.addr(word), v));
    }

    /// Starts a guest thread and returns its handle.
    pub fn spawn<F>(&self, name: &str, priority: i32, core: i32, f: F) -> u32
    where
        F: FnOnce(&Arc<Thread>) + Send + 'static,
    {
        let h = self
            .kernel
            .create_thread(ThreadSpec {
                name: name.to_string(),
                entry: 0,
                priority,
                core,
                stack_size: 0,
                body: Box::new(move || {
                    if let Some(t) = current_thread() {
                        f(&t);
                    }
                }),
            })
            .expect("create");
        self.kernel.start_thread(h).expect("start");
        h
    }

    /// Waits until every thread ended. Fails the test after 60 seconds, which means a lost wakeup.
    pub fn join(&self, handles: &[u32]) {
        for h in handles {
            let r = self.kernel.wait_synchronization(
                &self.main,
                &[*h],
                Timeout::After(Duration::from_secs(60)),
            );
            assert_eq!(r, Ok(0), "thread {h:#x} did not end");
        }
    }

    /// Polls until `cond` holds. Fails the test after 20 seconds.
    pub fn wait_until(&self, what: &str, mut cond: impl FnMut() -> bool) {
        let t0 = Instant::now();
        while !cond() {
            assert!(t0.elapsed() < Duration::from_secs(20), "timed out: {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The number of lock waits that have started.
    pub fn contended(&self) -> u64 {
        self.kernel.stats().iter().map(|s| s.contended_locks).sum()
    }

    /// The number of threads parked in a lock, condition variable or address wait now.
    pub fn blocked(&self) -> u64 {
        self.kernel.blocked_threads() as u64
    }
}

/// The SDK's mutex over the svc calls, as libnx writes it.
pub struct GuestMutex {
    pub kernel: Arc<Kernel>,
    pub mem: Arc<FakeMemory>,
    pub addr: u64,
}

impl GuestMutex {
    pub fn lock(&self, t: &Arc<Thread>) {
        let tag = t.handle();
        loop {
            let cur = self.mem.load32(self.addr).unwrap_or(0);
            if cur == 0 {
                if self.mem.cas32(self.addr, 0, tag) == Some(Ok(0)) {
                    return;
                }
                continue;
            }
            if cur & WAITERS_BIT == 0
                && self.mem.cas32(self.addr, cur, cur | WAITERS_BIT) != Some(Ok(cur))
            {
                continue;
            }
            assert_eq!(
                self.kernel
                    .arbitrate_lock(t, cur & !WAITERS_BIT, self.addr, tag),
                Ok(())
            );
            if self.mem.load32(self.addr).unwrap_or(0) & !WAITERS_BIT == tag {
                return;
            }
        }
    }

    pub fn unlock(&self, t: &Arc<Thread>) {
        let tag = t.handle();
        if self.mem.cas32(self.addr, tag, 0) != Some(Ok(tag)) {
            assert_eq!(self.kernel.arbitrate_unlock(self.addr), Ok(()));
        }
    }
}

/// The SDK's condition variable. The key word is the process-wide key.
pub struct GuestCondvar {
    pub key: u64,
}

impl GuestCondvar {
    pub fn wait(&self, m: &GuestMutex, t: &Arc<Thread>, timeout: Timeout) -> Result<(), KResult> {
        let r = m
            .kernel
            .wait_process_wide_key_atomic(t, m.addr, self.key, t.handle(), timeout);
        if r == Err(KResult::TIMED_OUT) {
            m.lock(t);
        }
        r
    }

    pub fn signal(&self, m: &GuestMutex, count: i32) {
        // The SDK reads the key first and enters the kernel only when threads wait.
        if m.mem.load32(self.key) != Some(0) {
            assert_eq!(m.kernel.signal_process_wide_key(self.key, count), Ok(()));
        }
    }
}
