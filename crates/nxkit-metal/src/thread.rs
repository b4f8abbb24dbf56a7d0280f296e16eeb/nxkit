//! The QoS class of host threads that encode for the GPU, their CPU clock, and a pool of encode threads.

/// Puts the calling thread in the user-interactive QoS class, so that the system runs it on a performance core. Threads
/// that encode a frame are on the frame's critical path; at the default class the system often put them on the
/// efficiency cores, and the caller waited for the slowest of them.
pub fn set_user_interactive() {
    #[cfg(target_vendor = "apple")]
    {
        const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
        }
        // SAFETY: the call changes only the class of the calling thread and takes plain values.
        let _ = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
    }
}

/// Puts the calling thread in the utility QoS class, for work that may wait behind the frame: the compile of a
/// pipeline that a draw waits for anyway. The class carries over to the system's shader compiler service, so it does
/// not take the cores of the threads that encode the frame.
pub fn set_utility() {
    #[cfg(target_vendor = "apple")]
    {
        const QOS_CLASS_UTILITY: u32 = 0x11;
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
        }
        // SAFETY: the call changes only the class of the calling thread and takes plain values.
        let _ = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) };
    }
}

/// The CPU time of the calling thread in nanoseconds, 0 where the platform has no clock for it. With the wall time it
/// tells whether a thread computed or waited (for a core or a lock).
pub fn cpu_time_ns() -> u64 {
    #[cfg(target_vendor = "apple")]
    {
        const CLOCK_THREAD_CPUTIME_ID: u32 = 16;
        unsafe extern "C" {
            fn clock_gettime_nsec_np(clock: u32) -> u64;
        }
        // SAFETY: the call reads a clock of the calling thread and takes a plain value.
        unsafe { clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID) }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        0
    }
}

/// A task of [`Pool::run`], as the helper threads see it. The pointer is valid while `Shared::task` holds it: `run` clears
/// it and waits until no helper runs it before the borrow ends.
#[derive(Clone, Copy)]
struct TaskPtr(*const (dyn Fn() + Sync));

// SAFETY: the task is `Sync`, and the pointer is used only while `Pool::run` keeps the task alive.
unsafe impl Send for TaskPtr {}

struct Shared {
    /// The task of the call in progress and how many helpers may still take it.
    task: Option<(TaskPtr, usize)>,
    /// Helpers that run the task now.
    running: usize,
    /// A helper's task panicked.
    panicked: bool,
}

/// Threads that live for the whole process and run a borrowed task on request. Threads made for one call started late
/// under load: in Breath of the Wild the chunks of a frame ran one after the other on the caller while the new threads
/// were still starting.
pub struct Pool {
    state: std::sync::Mutex<Shared>,
    work: std::sync::Condvar,
    done: std::sync::Condvar,
    started: std::sync::Mutex<usize>,
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Pool {
    /// The pool of the process. Its threads start on first use and run at the user-interactive QoS class.
    pub fn global() -> &'static Pool {
        static POOL: std::sync::OnceLock<Pool> = std::sync::OnceLock::new();
        POOL.get_or_init(|| Pool {
            state: std::sync::Mutex::new(Shared {
                task: None,
                running: 0,
                panicked: false,
            }),
            work: std::sync::Condvar::new(),
            done: std::sync::Condvar::new(),
            started: std::sync::Mutex::new(0),
        })
    }

    /// Runs `task` on the calling thread and on up to `helpers` pool threads at the same time, and returns when every
    /// one of them is done. The task decides itself how to share the work, for example through a queue. A panic of
    /// the task on any thread reaches the caller.
    pub fn run(&'static self, helpers: usize, task: &(dyn Fn() + Sync)) {
        if helpers == 0 {
            task();
            return;
        }
        self.grow(helpers);
        // SAFETY: only the lifetime is erased. `run` clears the task and waits until no helper runs it before it
        // returns, so no helper uses the pointer after the borrow of `task` ends.
        let ptr = TaskPtr(unsafe {
            std::mem::transmute::<*const (dyn Fn() + Sync + '_), *const (dyn Fn() + Sync + 'static)>(
                task,
            )
        });
        lock(&self.state).task = Some((ptr, helpers));
        self.work.notify_all();
        let mine = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
        let mut st = lock(&self.state);
        st.task = None;
        while st.running > 0 {
            st = self
                .done
                .wait(st)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let helper_panicked = std::mem::take(&mut st.panicked);
        drop(st);
        if let Err(p) = mine {
            std::panic::resume_unwind(p);
        }
        if helper_panicked {
            std::panic::resume_unwind(Box::new("a task of the pool panicked on a helper thread"));
        }
    }

    fn grow(&'static self, n: usize) {
        let mut started = lock(&self.started);
        while *started < n {
            let name = format!("nxkit-encode-{}", *started + 1);
            if std::thread::Builder::new()
                .name(name)
                .spawn(move || self.helper())
                .is_err()
            {
                return;
            }
            *started += 1;
        }
    }

    fn helper(&'static self) {
        set_user_interactive();
        loop {
            let ptr = {
                let mut st = lock(&self.state);
                loop {
                    if let Some((ptr, left)) = st.task.as_mut()
                        && *left > 0
                    {
                        *left -= 1;
                        let ptr = *ptr;
                        st.running += 1;
                        break ptr;
                    }
                    st = self
                        .work
                        .wait(st)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            };
            // SAFETY: `run` keeps the task alive until `running` is back to zero.
            let ok =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { (*ptr.0)() }))
                    .is_ok();
            let mut st = lock(&self.state);
            st.running -= 1;
            st.panicked |= !ok;
            drop(st);
            self.done.notify_all();
        }
    }
}
