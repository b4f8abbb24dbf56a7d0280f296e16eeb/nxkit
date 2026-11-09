//! The creation of a thread object: the stack, the entry and the thread record.

use super::*;

impl Kernel {
    pub(super) fn new_thread(
        &self,
        spec: &ThreadSpec,
        state: ThreadState,
    ) -> KResultOr<Arc<Thread>> {
        Self::check_priority(spec.priority)?;
        let core = match spec.core {
            -2 => 0,
            c if (0..64).contains(&c) && self.config.core_mask & 1 << c != 0 => c,
            _ => return Err(KResult::INVALID_CORE_ID),
        };
        let t = Arc::new(Thread::new(
            self.next_id.fetch_add(1, Ordering::Relaxed),
            &spec.name,
            spec.entry,
            spec.priority,
            core,
            self.config.core_mask,
            state,
        ));
        t.handle.store(
            self.handles.insert(KObject::Thread(t.clone()))?,
            Ordering::Relaxed,
        );
        self.assign_role(&t);
        lock(&self.threads).push(t.clone());
        Ok(t)
    }

    /// Registers the calling host thread as a started guest thread, for the host's main thread.
    pub fn attach_current_thread(
        &self,
        name: &str,
        priority: i32,
        core: i32,
    ) -> KResultOr<Arc<Thread>> {
        let spec = ThreadSpec {
            name: name.to_string(),
            entry: 0,
            priority,
            core,
            stack_size: 0,
            body: Box::new(|| {}),
        };
        let t = self.new_thread(&spec, ThreadState::Started)?;
        if let Err(e) = self.tls_attach(&t) {
            self.exit_thread(&t);
            return Err(e);
        }
        CURRENT.with(|c| *c.borrow_mut() = Some(t.clone()));
        t.lend.open(sys::current_host_thread());
        self.apply_qos(&t);
        self.gates.enter(&t);
        Ok(t)
    }

    /// Registers the calling host thread as the main thread, with the priority, core and stack size of the NPDM
    /// from [`Config::main_thread`]. Without them the call fails and says so, and it never guesses a priority.
    pub fn attach_main_thread(&self, name: &str) -> KResultOr<Arc<Thread>> {
        let Some(main) = self.config.main_thread else {
            (self.config.log)(
                "the main thread cannot attach, the configuration has no main_thread from the NPDM",
            );
            return Err(KResult::INVALID_STATE);
        };
        let t = self.attach_current_thread(name, main.priority, main.core)?;
        t.stack_size.store(
            usize::try_from(main.stack_size).unwrap_or(usize::MAX),
            Ordering::Relaxed,
        );
        Ok(t)
    }

    /// `CreateThread`: returns the handle. The thread runs after `start_thread`.
    pub fn create_thread(&self, spec: ThreadSpec) -> KResultOr<u32> {
        let body = spec.body;
        let spec = ThreadSpec {
            body: Box::new(|| {}),
            ..spec
        };
        let t = self.new_thread(&spec, ThreadState::Created)?;
        *lock(&t.body) = Some(body);
        t.stack_size
            .store(stack_size(spec.stack_size), Ordering::Relaxed);
        Ok(t.handle())
    }

    /// `StartThread`.
    pub fn start_thread(&self, handle: u32) -> KResultOr<()> {
        let t = self.thread_by_handle(handle)?;
        if t.state() != ThreadState::Created {
            return Err(KResult::INVALID_STATE);
        }
        let Some(kernel) = self.me.upgrade() else {
            return Err(KResult::INVALID_STATE);
        };
        t.set_state(ThreadState::Started);
        let body = lock(&t.body).take();
        let name = t.name();
        let th = t.clone();
        let spawned = std::thread::Builder::new()
            .name(if name.is_empty() {
                format!("guest-{:#x}", t.id)
            } else {
                name
            })
            .stack_size(t.stack_size.load(Ordering::Relaxed))
            .spawn(move || {
                CURRENT.with(|c| *c.borrow_mut() = Some(th.clone()));
                let _exit = ExitGuard(kernel.clone(), th.clone());
                if let Err(e) = kernel.tls_attach(&th) {
                    (kernel.config.log)(&format!(
                        "thread {:#x} does not start, it has no thread-local slot: {e:?}",
                        th.id
                    ));
                    return;
                }
                th.lend.open(sys::current_host_thread());
                kernel.apply_qos(&th);
                kernel.gates.enter(&th);
                if let Some(body) = body {
                    body();
                }
            });
        if spawned.is_err() {
            t.set_state(ThreadState::Created);
            return Err(KResult::OUT_OF_RESOURCE);
        }
        Ok(())
    }

    /// `ExitThread`. Marks the thread as exited and signals the threads that wait for it. The host's svc gate then
    /// ends the host thread. Calling it twice does nothing. The lend table closes first, while the host thread still
    /// runs (see `lend.rs`), and the thread-local slot goes back to the free list. The thread must not read its
    /// thread-local region after the call.
    pub fn exit_thread(&self, cur: &Arc<Thread>) {
        if cur.state() == ThreadState::Exited {
            return;
        }
        self.gates.leave(cur);
        cur.lend.close();
        cur.set_state(ThreadState::Exited);
        self.tls_release(cur);
        cur.exit.signal();
    }
}
