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

    /// Writes an 8-byte word as two 4-byte words, the low one first. The kernel uses it for structures that the guest
    /// does not read while the kernel writes them, such as `MemoryInfo` and the thread-local region.
    fn store64(&self, addr: u64, value: u64) -> bool {
        self.store32(addr, value as u32) && self.store32(addr + 4, (value >> 32) as u32)
    }

    /// Reads `out.len()` bytes. Every byte comes from the aligned word that holds it, so a read never leaves the page
    /// of its last byte.
    fn read_bytes(&self, addr: u64, out: &mut [u8]) -> bool {
        for (i, b) in out.iter_mut().enumerate() {
            let at = addr.wrapping_add(i as u64);
            match self.load32(at & !3) {
                Some(w) => *b = (w >> (8 * (at & 3))) as u8,
                None => return false,
            }
        }
        true
    }
}

/// The Horizon page size. Every address and size of a memory svc is a multiple of it.
pub const PAGE_SIZE: u64 = 0x1000;
/// The operations of the host on its one reservation of guest memory (R18). The kernel keeps its own model of what
/// each range is. This trait makes the host's pages match that model.
///
/// Horizon's pages are 4 KiB and the host's may be larger (16 KiB on Apple silicon). `protect` rounds a range
/// outward when it grants access and inward when it removes it, so a host page never loses access that one of its
/// Horizon pages still has. `release` and `zero` work on any 4 KiB range. `remap` shares pages and so needs
/// host-page alignment, which the kernel checks with [`HostSpace::page_size`].
pub trait HostSpace: Send + Sync {
    /// The host page size in bytes, a power of two that is at least [`PAGE_SIZE`].
    fn page_size(&self) -> u64;

    /// Sets the access of the pages of `addr..addr + len` to `perm` (the Horizon permission bits: 1 read, 2 write,
    /// 4 execute). Returns false when the host refuses.
    fn protect(&self, addr: u64, len: u64, perm: u32) -> bool;

    /// Gives the pages back: they read as zero when they are made accessible again, and they are not accessible now.
    /// Pages that share a host page with memory that stays are zeroed instead.
    fn release(&self, addr: u64, len: u64) -> bool;

    /// Writes zeros. The pages stay as accessible as they are.
    fn zero(&self, addr: u64, len: u64) -> bool;

    /// Maps the pages of `src..src + len` at `dst..dst + len` so that both ranges see the same memory, with the
    /// access `perm` at `dst`. `dst` and `src` and `len` are host-page aligned.
    fn remap(&self, dst: u64, src: u64, len: u64, perm: u32) -> bool;

    /// Copies `len` bytes. The ranges do not overlap.
    fn copy(&self, dst: u64, src: u64, len: u64) -> bool;

    /// True when `remap` and `release` may put other pages under `addr..addr + len`. False where the pages must stay
    /// (`crate::pinned`): there `MapMemory` copies, and `release` zeroes the pages in place.
    fn can_remap(&self, _addr: u64, _len: u64) -> bool {
        true
    }

    /// Takes away a view that `remap` made, without writing to the pages it shows (they belong to the source). The
    /// pages are not accessible afterwards.
    fn drop_view(&self, addr: u64, len: u64) -> bool {
        self.release(addr, len)
    }
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

impl FakeMemory {
    fn fill(&self, addr: u64, len: u64, value: u32) -> bool {
        (0..len / 4).all(|i| self.store32(addr + 4 * i, value))
    }
}

/// The fake has no protection. `remap` copies, so the two ranges do not stay equal afterwards. Tests that need
/// shared pages use the host memory.
impl HostSpace for FakeMemory {
    fn page_size(&self) -> u64 {
        PAGE_SIZE
    }

    fn protect(&self, addr: u64, len: u64, _perm: u32) -> bool {
        self.word(addr).is_some() && self.word(addr + len.saturating_sub(4)).is_some()
    }

    fn release(&self, addr: u64, len: u64) -> bool {
        self.fill(addr, len, 0)
    }

    fn zero(&self, addr: u64, len: u64) -> bool {
        self.fill(addr, len, 0)
    }

    fn remap(&self, dst: u64, src: u64, len: u64, _perm: u32) -> bool {
        self.copy(dst, src, len)
    }

    fn copy(&self, dst: u64, src: u64, len: u64) -> bool {
        (0..len / 4).all(|i| {
            self.load32(src + 4 * i)
                .is_some_and(|w| self.store32(dst + 4 * i, w))
        })
    }
}
