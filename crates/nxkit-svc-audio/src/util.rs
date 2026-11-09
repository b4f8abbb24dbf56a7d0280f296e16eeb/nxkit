//! Small helpers for the take-overs: maps from guest object to host state, and writes of names into guest memory.

use nxkit_abi::guest;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Entries of an [`IdxMap`]. More than the outputs of the mixer.
const IDX_CAP: usize = 16;

/// A lock-free map from the guest address of an SDK object (an `AudioOut`, an `OutputStream`) to a small index. A call
/// of the game on such an object looks it up, so it takes no lock (R7). Address 0 is not a key.
pub(crate) struct IdxMap {
    keys: [AtomicU64; IDX_CAP],
    vals: [AtomicUsize; IDX_CAP],
}

impl IdxMap {
    /// An empty map.
    pub(crate) const fn new() -> IdxMap {
        IdxMap {
            keys: [const { AtomicU64::new(0) }; IDX_CAP],
            vals: [const { AtomicUsize::new(0) }; IDX_CAP],
        }
    }

    /// The entry of `k`. The search starts at the home entry of the key, so a hit usually costs one load. A removed
    /// entry is only a hole, and the search covers the whole table, so a hole never hides a key.
    fn find(&self, k: u64) -> Option<usize> {
        let home = ((k >> 4) as usize) % IDX_CAP;
        (0..IDX_CAP)
            .map(|n| (home + n) % IDX_CAP)
            .find(|&i| self.keys[i].load(Ordering::Acquire) == k)
    }

    /// Stores `v` under `k`. Returns false when the map is full. A key is only used after its `put` returned, which is
    /// how a game uses an object that it opened.
    pub(crate) fn put(&self, k: u64, v: usize) -> bool {
        if k == 0 {
            return false;
        }
        if let Some(i) = self.find(k) {
            self.vals[i].store(v, Ordering::Release);
            return true;
        }
        let home = ((k >> 4) as usize) % IDX_CAP;
        for n in 0..IDX_CAP {
            let i = (home + n) % IDX_CAP;
            if self.keys[i]
                .compare_exchange(0, k, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.vals[i].store(v, Ordering::Release);
                return true;
            }
        }
        false
    }

    /// The index under `k`.
    pub(crate) fn get(&self, k: u64) -> Option<usize> {
        if k == 0 {
            return None;
        }
        let i = self.find(k)?;
        Some(self.vals[i].load(Ordering::Acquire))
    }

    /// Removes `k` and returns its index.
    pub(crate) fn del(&self, k: u64) -> Option<usize> {
        if k == 0 {
            return None;
        }
        let i = self.find(k)?;
        let v = self.vals[i].load(Ordering::Acquire);
        self.keys[i].store(0, Ordering::Release);
        Some(v)
    }
}

/// A map keyed by the guest address of an SDK object, behind one lock. It is for open and close only. A call that a
/// game makes every frame uses an [`IdxMap`].
pub(crate) struct ObjMap<T> {
    m: Mutex<BTreeMap<u64, T>>,
}

impl<T: Clone> ObjMap<T> {
    /// An empty map.
    pub(crate) const fn new() -> ObjMap<T> {
        ObjMap {
            m: Mutex::new(BTreeMap::new()),
        }
    }

    /// Stores `v` under `k`.
    pub(crate) fn put(&self, k: u64, v: T) {
        if let Ok(mut m) = self.m.lock() {
            m.insert(k, v);
        }
    }

    /// Removes and returns the value under `k`.
    pub(crate) fn del(&self, k: u64) -> Option<T> {
        self.m.lock().ok().and_then(|mut m| m.remove(&k))
    }
}

/// Adds the export `name` for the `extern "C"` function `f` to the list `v`.
macro_rules! ex {
    ($v:expr, $name:expr, $f:expr) => {
        $v.push(nxkit_abi::export::Export::new(
            $name,
            $f as *const () as usize,
        ))
    };
}
pub(crate) use ex;

/// The name of the one output device of AudioOut, with its NUL. Its address is a valid pointer for the guest.
pub(crate) static DEVICE_OUT: &[u8] = b"DeviceOut\0";

/// Pads the arguments of a call into the SDK to the eight registers that `sys::call::call` takes.
pub(crate) fn args(a: &[u64]) -> [u64; 8] {
    let mut o = [0u64; 8];
    for (d, s) in o.iter_mut().zip(a) {
        *d = *s;
    }
    o
}

/// The result code of a generic failure. nxnative: `RES_FAIL`.
pub(crate) const RES_FAIL: u32 = 0x2A2;

/// Writes `s` and NUL padding so that `size` bytes at `addr` are set.
pub(crate) fn put_name(addr: u64, s: &str, size: usize) {
    let mut v = vec![0u8; size];
    let n = s.len().min(size.saturating_sub(1));
    v[..n].copy_from_slice(&s.as_bytes()[..n]);
    let _ = guest::write(addr, &v);
}
