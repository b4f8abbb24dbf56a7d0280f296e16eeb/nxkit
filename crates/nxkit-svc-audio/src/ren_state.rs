//! The state of the SDK renderers that the host opened: up to four, found by slot, by handle or by system object.

use crate::host;
use crate::ren_cmd::{Cap, Layout};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

/// What one renderer keeps between its frames: the command layout it learned and its render time figures.
pub(crate) struct Lay {
    pub(crate) layout: Layout,
    pub(crate) sink_set: bool,
    pub(crate) sum: Duration,
    pub(crate) max: Duration,
    pub(crate) count: u32,
}

impl Lay {
    fn new() -> Lay {
        Lay {
            layout: Layout::new(),
            sink_set: false,
            sum: Duration::ZERO,
            max: Duration::ZERO,
            count: 0,
        }
    }
}

/// Renderers open at the same time.
pub(crate) const AR_MAX: usize = 4;
/// Catch-up frames that wait for the render thread.
pub(crate) const CQ: usize = 4;

/// Locks a mutex. A thread that panicked does not stop the audio.
pub(crate) fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The address of one SDK function, looked up on first use. A missing function stops the game.
pub(crate) struct Sym {
    cell: OnceLock<usize>,
    name: &'static str,
}

impl Sym {
    /// A lookup of the SDK function with this mangled name.
    pub(crate) const fn new(name: &'static str) -> Sym {
        Sym {
            cell: OnceLock::new(),
            name,
        }
    }

    /// The address of the SDK's own function.
    pub(crate) fn addr(&self) -> usize {
        *self.cell.get_or_init(|| host::sym(self.name))
    }
}

/// One open renderer.
pub(crate) struct Ren {
    pub(crate) idx: usize,
    /// The two words of the `AudioRendererHandle`.
    pub(crate) h0: u64,
    pub(crate) h1: u64,
    /// Its `AudioRenderSystem`, 0 until the first render or update names it.
    pub(crate) sys: AtomicU64,
    pub(crate) open: AtomicBool,
    /// The render thread, 0 when there is none.
    pub(crate) tt: AtomicU64,
    /// The latest frame, stereo 16-bit. Empty when the render made no sound.
    pub(crate) frame: Mutex<Vec<i16>>,
    /// Frames rendered by an update, which the render thread plays first.
    pub(crate) cq: Mutex<VecDeque<Vec<i16>>>,
    pub(crate) cq_cv: Condvar,
    pub(crate) cap: Mutex<Cap>,
    /// The command layout and the timing of this renderer. Only the thread that renders takes it.
    pub(crate) lay: Mutex<Lay>,
    /// Held for a whole render: command generation, processing and the read of the output. The render thread and a
    /// catch-up render on the game's update thread share the renderer's command buffer; when one generated a new list
    /// while the other still processed or read its own, that list came out malformed and its 5 ms block silent, which
    /// clicked.
    pub(crate) render: Mutex<()>,
    pub(crate) renders: AtomicU64,
    pub(crate) updates: AtomicU64,
    pub(crate) catchups: AtomicU64,
    pub(crate) last_r: AtomicU64,
    pub(crate) updated: AtomicBool,
}

impl Ren {
    /// A renderer for slot `idx` with this handle.
    pub(crate) fn new(idx: usize, h0: u64, h1: u64) -> Ren {
        Ren {
            idx,
            h0,
            h1,
            sys: AtomicU64::new(0),
            open: AtomicBool::new(true),
            tt: AtomicU64::new(0),
            frame: Mutex::new(Vec::new()),
            cq: Mutex::new(VecDeque::new()),
            cq_cv: Condvar::new(),
            cap: Mutex::new(Cap::new()),
            lay: Mutex::new(Lay::new()),
            render: Mutex::new(()),
            renders: AtomicU64::new(0),
            updates: AtomicU64::new(0),
            catchups: AtomicU64::new(0),
            last_r: AtomicU64::new(0),
            updated: AtomicBool::new(false),
        }
    }

    /// Takes the oldest catch-up frame and tells a waiting update that there is room.
    pub(crate) fn pop_cq(&self) -> Option<Vec<i16>> {
        let f = lk(&self.cq).pop_front();
        if f.is_some() {
            self.cq_cv.notify_all();
        }
        f
    }

    /// Wakes an update that waits for room in the catch-up queue.
    pub(crate) fn wake_updates(&self) {
        let _g = lk(&self.cq);
        self.cq_cv.notify_all();
    }

    /// Whether the renderer is open.
    pub(crate) fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
}

/// The open renderers by slot.
pub(crate) static SLOTS: Mutex<[Option<Arc<Ren>>; AR_MAX]> = Mutex::new([const { None }; AR_MAX]);

thread_local! {
    /// The renderer that this render thread drives.
    pub(crate) static T_AR: RefCell<Option<Arc<Ren>>> = const { RefCell::new(None) };
    /// True while the SDK's open runs, so that its nested open goes straight to the SDK.
    pub(crate) static T_IN_OPEN: Cell<bool> = const { Cell::new(false) };
    /// The last system and renderer that [`of_sys`] answered on this thread.
    static T_SYS: RefCell<Option<(u64, Arc<Ren>)>> = const { RefCell::new(None) };
}

/// The renderer in slot `idx`.
pub(crate) fn slot(idx: usize) -> Option<Arc<Ren>> {
    lk(&SLOTS).get(idx).and_then(|s| s.clone())
}

/// The renderer that has this handle word.
pub(crate) fn by_handle(h0: u64) -> Option<Arc<Ren>> {
    lk(&SLOTS).iter().flatten().find(|r| r.h0 == h0).cloned()
}

/// Frees the slot of `r`.
pub(crate) fn free_slot(r: &Arc<Ren>) {
    let mut s = lk(&SLOTS);
    if let Some(e) = s.get_mut(r.idx) {
        if e.as_ref().is_some_and(|x| Arc::ptr_eq(x, r)) {
            *e = None;
        }
    }
}

/// The renderer an `AudioRenderSystem` belongs to, for the SDK calls that name only that. A thread that calls this
/// every frame (the game's update) remembers its last answer, so only the first call and a change of renderer take
/// the lock of the slots.
pub(crate) fn of_sys(sys: u64) -> Option<Arc<Ren>> {
    let hit = T_SYS.with(|c| {
        c.borrow()
            .as_ref()
            .filter(|(s, r)| *s == sys && r.is_open())
            .map(|(_, r)| r.clone())
    });
    if hit.is_some() {
        return hit;
    }
    let r = lookup_sys(sys)?;
    T_SYS.with(|c| *c.borrow_mut() = Some((sys, r.clone())));
    Some(r)
}

/// The renderer in the slots whose system is `sys`. A system not seen yet belongs to the one open renderer whose system
/// is not known: the render thread names its system at its first render, and a game's first updates can come before that.
fn lookup_sys(sys: u64) -> Option<Arc<Ren>> {
    let slots = lk(&SLOTS);
    let mut unknown: Option<&Arc<Ren>> = None;
    let mut count = 0;
    for r in slots.iter().flatten() {
        let s = r.sys.load(Ordering::Acquire);
        if s == sys {
            return Some(r.clone());
        }
        if s == 0 {
            unknown = Some(r);
            count += 1;
        }
    }
    match unknown {
        Some(r) if count == 1 => {
            r.sys.store(sys, Ordering::Release);
            Some(r.clone())
        }
        _ => None,
    }
}
