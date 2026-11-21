//! A report of every guest thread, for a diagnostic of the host (a stall of the game): its state, whether it waits
//! in a lock, and, for a thread in a svc, the guest's pc and the frame chain of its stack.

use crate::kernel::Kernel;
use crate::sync::lock;
use std::fmt::Write;

/// The frames of a chain that the report follows.
const CHAIN: usize = 12;

impl Kernel {
    /// One line per guest thread. A thread that runs guest code shows no registers, because only the svc gate saves
    /// them. It reads the gate frame of a thread in a svc as `GetThreadContext3` does, so the thread cannot leave the
    /// svc while it is read.
    pub fn thread_report(&self) -> String {
        let threads: Vec<_> = lock(&self.threads).iter().cloned().collect();
        let mut out = String::new();
        for t in threads {
            let _ = write!(
                out,
                "thread {:#x} {:?} prio {} {:?}{}",
                t.id,
                t.name(),
                t.priority(),
                t.state(),
                if t.stats.blocked.load(std::sync::atomic::Ordering::Relaxed) {
                    " blocked"
                } else {
                    ""
                }
            );
            let ctx = self.config.threads.as_deref().and_then(|control| {
                let frame = t.activity.begin_read()?;
                let ctx = control.gate_context(frame);
                t.activity.end_read();
                Some(ctx)
            });
            match ctx {
                None => out.push_str(" in guest code\n"),
                Some(c) => {
                    let _ = write!(out, " in svc: pc {:#x} lr {:#x} chain", c.pc, c.lr);
                    let mut fp = c.fp;
                    for _ in 0..CHAIN {
                        if fp == 0 || !fp.is_multiple_of(8) {
                            break;
                        }
                        let (Some(next), Some(ret)) =
                            (self.mem.load64(fp), self.mem.load64(fp + 8))
                        else {
                            break;
                        };
                        let _ = write!(out, " {ret:#x}");
                        if next <= fp {
                            break;
                        }
                        fp = next;
                    }
                    out.push('\n');
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, FakeMemory, Kernel, MainThread};
    use std::sync::Arc;

    #[test]
    fn every_thread_has_a_line() {
        let config = Config {
            main_thread: Some(MainThread {
                priority: 28,
                core: 0,
                stack_size: 0x1000,
            }),
            log: |_| {},
            ..Config::default()
        };
        let k = Kernel::new(config, Arc::new(FakeMemory::new(0x10_0000, 16)));
        assert!(k.thread_report().is_empty());
        assert!(k.attach_main_thread("main").is_ok());
        let r = k.thread_report();
        assert_eq!(r.lines().count(), 1);
        assert!(r.contains("\"main\" prio 28"), "{r}");
        assert!(r.ends_with("in guest code\n"), "{r}");
    }
}
