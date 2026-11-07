//! Guest memory as the kernel sees it: aligned words with atomic access.

use std::sync::atomic::{AtomicU32, Ordering};

/// Word access to the guest's address space. An address the kernel may not touch gives `None` or `false`, which
/// the svc turns into `InvalidCurrentMemory`. The user-mode fast paths of the SDK use the same words with their own
/// atomic operations, so every access here is atomic and sequentially consistent.
pub trait GuestMemory: Send + Sync {
    /// Reads a 4-byte word.
    fn load32(&self, addr: u64) -> Option<u32>;
    /// Reads an 8-byte word.
    fn load64(&self, addr: u64) -> Option<u64>;
    /// Writes a 4-byte word. Returns false when the address is not accessible.
    fn store32(&self, addr: u64, value: u32) -> bool;
    /// Compare and swap. `Some(Ok(old))` swapped, `Some(Err(seen))` did not, `None` is not accessible.
    fn cas32(&self, addr: u64, current: u32, new: u32) -> Option<Result<u32, u32>>;
}

/// Guest memory backed by a vector of words, for tests and tools. Addresses start at `base`.
pub struct FakeMemory {
    base: u64,
    words: Vec<AtomicU32>,
}

impl FakeMemory {
    /// `words` zeroed words from `base`, which must be 8-aligned.
    pub fn new(base: u64, words: usize) -> FakeMemory {
        FakeMemory {
            base,
            words: (0..words).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    /// The address of word number `index`.
    pub fn addr(&self, index: usize) -> u64 {
        self.base + 4 * index as u64
    }

    fn word(&self, addr: u64) -> Option<&AtomicU32> {
        let off = addr.checked_sub(self.base)?;
        if off % 4 != 0 {
            return None;
        }
        self.words.get(usize::try_from(off / 4).ok()?)
    }
}

impl GuestMemory for FakeMemory {
    fn load32(&self, addr: u64) -> Option<u32> {
        Some(self.word(addr)?.load(Ordering::SeqCst))
    }

    fn load64(&self, addr: u64) -> Option<u64> {
        if !addr.is_multiple_of(8) {
            return None;
        }
        let lo = self.word(addr)?.load(Ordering::SeqCst);
        let hi = self.word(addr + 4)?.load(Ordering::SeqCst);
        Some(u64::from(lo) | u64::from(hi) << 32)
    }

    fn store32(&self, addr: u64, value: u32) -> bool {
        self.word(addr)
            .map(|w| w.store(value, Ordering::SeqCst))
            .is_some()
    }

    fn cas32(&self, addr: u64, current: u32, new: u32) -> Option<Result<u32, u32>> {
        Some(
            self.word(addr)?
                .compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst),
        )
    }
}
