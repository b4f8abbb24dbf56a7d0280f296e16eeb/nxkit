//! The loaded patches and the dispatch of the hook points (W38).
//!
//! The encoder asks [`active`] first. That is one relaxed load of an atomic, so a run without a patch pays nothing
//! else. A hook then takes the read lock of the list and calls each patch in load order.
//!
//! **Unload safety.** A patch call runs with the read lock held. [`remove`] and [`install`] (which replaces) take the
//! write lock. They return only when no thread is inside a patch call, and no thread can enter one of the removed
//! patches after that: the entry is gone from the list. The caller drops the returned [`Entry`] after the call, and the
//! drop of its `keep` value does the `dlclose`. The lock is not held then. A patch must not keep a thread, a callback
//! or a pointer of its own past the unload, because its code can go away.

use std::any::Any;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{PoisonError, RwLock};

use crate::answer::{DrawAnswer, PassAnswer};
use crate::table::PatchTable;
use crate::types::{DrawInfo, FrameBegin, FrameEnd, PassInfo};

pub use crate::table::{HOOK_DRAW, HOOK_FRAME, HOOK_PASS};

/// The OR of the hooks of all loaded patches. 0 when none is loaded. The reason for the static: the encoder threads
/// have no host context, and the check must cost one load.
static ACTIVE: AtomicU32 = AtomicU32::new(0);

/// The loaded patches in load order. The reason for the static: as above, and the control thread changes it.
static PATCHES: RwLock<Vec<Entry>> = RwLock::new(Vec::new());

/// A loaded patch.
pub struct Entry {
    /// The name from its table.
    pub name: String,
    /// The path that it was loaded from, for `patch reload`.
    pub path: String,
    /// Its hooks.
    pub table: PatchTable,
    /// What keeps the code mapped. Its drop unloads the library.
    pub keep: Box<dyn Any + Send + Sync>,
}

/// What `patch list` shows of a loaded patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The name.
    pub name: String,
    /// The path that it was loaded from.
    pub path: String,
    /// The hooks that it has, as `HOOK_*` bits.
    pub hooks: u32,
}

fn recompute(list: &[Entry]) {
    let mask = list.iter().fold(0, |m, e| m | e.table.hook_mask());
    ACTIVE.store(mask, Ordering::Release);
}

/// True when a loaded patch has one of the hooks in `mask`. One relaxed atomic load.
#[inline]
pub fn active(mask: u32) -> bool {
    ACTIVE.load(Ordering::Relaxed) & mask != 0
}

/// Adds a patch. A patch of the same name is replaced in its place, with no moment in between where neither runs.
/// The replaced entry comes back: drop it after this call returns, when no lock is held.
#[must_use = "drop the replaced entry, its drop unloads the old library"]
pub fn install(entry: Entry) -> Option<Entry> {
    let mut list = PATCHES.write().unwrap_or_else(PoisonError::into_inner);
    let old = match list.iter().position(|e| e.name == entry.name) {
        Some(i) => Some(std::mem::replace(&mut list[i], entry)),
        None => {
            list.push(entry);
            None
        }
    };
    recompute(&list);
    old
}

/// Takes out the patch `name`, or all patches for `None`. The entries come back: drop them after this call returns.
#[must_use = "drop the entries, their drop unloads the libraries"]
pub fn remove(name: Option<&str>) -> Vec<Entry> {
    let mut list = PATCHES.write().unwrap_or_else(PoisonError::into_inner);
    let mut out = Vec::new();
    let mut i = 0;
    while i < list.len() {
        if name.is_none_or(|n| list[i].name == n) {
            out.push(list.remove(i));
        } else {
            i += 1;
        }
    }
    recompute(&list);
    out
}

/// The loaded patches in load order.
pub fn list() -> Vec<Listed> {
    let list = PATCHES.read().unwrap_or_else(PoisonError::into_inner);
    list.iter()
        .map(|e| Listed {
            name: e.name.clone(),
            path: e.path.clone(),
            hooks: e.table.hook_mask(),
        })
        .collect()
}

/// Calls the frame begin hooks.
pub fn frame_begin(info: &FrameBegin) {
    let list = PATCHES.read().unwrap_or_else(PoisonError::into_inner);
    for f in list.iter().filter_map(|e| e.table.frame_begin) {
        f(info);
    }
}

/// Calls the frame end hooks.
pub fn frame_end(info: &FrameEnd) {
    let list = PATCHES.read().unwrap_or_else(PoisonError::into_inner);
    for f in list.iter().filter_map(|e| e.table.frame_end) {
        f(info);
    }
}

/// Calls the pass hooks and merges their answers.
pub fn pass(info: &PassInfo) -> PassAnswer {
    let list = PATCHES.read().unwrap_or_else(PoisonError::into_inner);
    let mut out = PassAnswer::default();
    for f in list.iter().filter_map(|e| e.table.pass) {
        let mut a = PassAnswer::default();
        f(info, &mut a);
        out.merge(&a);
    }
    out
}

/// Calls the draw hooks and merges their answers.
pub fn draw(info: &DrawInfo) -> DrawAnswer {
    let list = PATCHES.read().unwrap_or_else(PoisonError::into_inner);
    let mut out = DrawAnswer::default();
    for f in list.iter().filter_map(|e| e.table.draw) {
        let mut a = DrawAnswer::default();
        f(info, &mut a);
        out.merge(&a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answer::DRAW_SKIP;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// The registry is one process-wide list, so the tests that change it take turns.
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    extern "C" fn skip_all(_: &DrawInfo, a: &mut DrawAnswer) {
        a.skip();
    }
    extern "C" fn frame(_: &FrameBegin) {}

    /// Sets a flag when it drops, as the `dlclose` guard of the host does.
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn entry(name: &str, draw: bool, flag: &Arc<AtomicBool>) -> Entry {
        let mut table = PatchTable::default();
        table.set_name(name);
        if draw {
            table.draw = Some(skip_all);
        } else {
            table.frame_begin = Some(frame);
        }
        Entry {
            name: name.into(),
            path: format!("/tmp/{name}.dylib"),
            table,
            keep: Box::new(Guard(flag.clone())),
        }
    }

    #[test]
    fn install_dispatch_replace_and_remove() {
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let (a_gone, b_gone) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(!active(HOOK_DRAW | HOOK_PASS | HOOK_FRAME));
        assert_eq!(draw(&DrawInfo::default()).flags, 0);

        assert!(install(entry("a", true, &a_gone)).is_none());
        assert!(install(entry("b", false, &b_gone)).is_none());
        assert!(active(HOOK_DRAW) && active(HOOK_FRAME) && !active(HOOK_PASS));
        assert_eq!(draw(&DrawInfo::default()).flags, DRAW_SKIP);
        let names: Vec<_> = list().into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["a", "b"]);

        // The same name replaces in place, and the old library is unloaded when the caller drops the entry.
        let a2 = Arc::new(AtomicBool::new(false));
        let old = install(entry("a", false, &a2));
        assert!(old.is_some());
        assert!(!a_gone.load(Ordering::SeqCst));
        drop(old);
        assert!(a_gone.load(Ordering::SeqCst));
        assert!(!active(HOOK_DRAW));
        assert_eq!(list().len(), 2);

        drop(remove(Some("zzz")));
        assert_eq!(list().len(), 2);
        drop(remove(Some("a")));
        assert!(a2.load(Ordering::SeqCst) && !b_gone.load(Ordering::SeqCst));
        drop(remove(None));
        assert!(b_gone.load(Ordering::SeqCst));
        assert!(list().is_empty());
        assert!(!active(HOOK_DRAW | HOOK_PASS | HOOK_FRAME));
    }

    #[test]
    fn a_remove_waits_for_a_call_in_progress() {
        use std::sync::mpsc;
        use std::time::Duration;
        static ENTERED: AtomicBool = AtomicBool::new(false);
        static RELEASE: AtomicBool = AtomicBool::new(false);
        extern "C" fn slow(_: &DrawInfo, _: &mut DrawAnswer) {
            ENTERED.store(true, Ordering::SeqCst);
            while !RELEASE.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let gone = Arc::new(AtomicBool::new(false));
        let mut e = entry("slow", true, &gone);
        e.table.draw = Some(slow);
        assert!(install(e).is_none());
        let caller = std::thread::spawn(|| draw(&DrawInfo::default()));
        while !ENTERED.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let (tx, rx) = mpsc::channel();
        let remover = std::thread::spawn(move || {
            drop(remove(Some("slow")));
            let _ = tx.send(());
        });
        // The remove cannot finish while the call runs.
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(!gone.load(Ordering::SeqCst));
        RELEASE.store(true, Ordering::SeqCst);
        assert!(caller.join().is_ok());
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        assert!(remover.join().is_ok());
        assert!(gone.load(Ordering::SeqCst));
    }
}
