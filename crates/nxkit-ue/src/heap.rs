//! An allocator over one range of guest addresses, with its bookkeeping on the host.
//!
//! Small blocks (up to [`SMALL_MAX`]) come from size classes, each class cut out of spans of [`SPAN`] bytes: a block of
//! class `c` sits at a multiple of `c` from its span's start, so a class whose size is a multiple of the alignment
//! gives that alignment. The free blocks of a class are a stack of addresses. A span stays with its class. Large
//! blocks are runs of [`GRAIN`] bytes from a best-fit free list with coalescing. A block of 64 KiB or more is 64 KiB
//! aligned, as Unreal's virtual allocator gives span-sized blocks (Wwise masks an interior pointer to that boundary to
//! find its header). The heap never writes the guest memory it hands out.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// The span of the size classes, and the alignment of the range.
pub const SPAN: u64 = 0x1_0000;
/// The largest small block.
pub const SMALL_MAX: u64 = 0x8000;
/// The unit of a large block.
pub const GRAIN: u64 = 0x4000;
/// The alignment of a block of [`SPAN`] bytes or more.
const BIG_ALIGN: u64 = SPAN;
/// The default alignment.
const MIN_ALIGN: u64 = 16;
/// A new peak of this many bytes more is logged.
const PEAK_STEP: u64 = 1 << 28;

/// The sizes of the classes: 16 to 128 in steps of 16, then four steps per doubling up to [`SMALL_MAX`].
fn class_sizes() -> Vec<u64> {
    let mut v: Vec<u64> = (1..=8).map(|i| i * 16).collect();
    let mut p = 128;
    while p < SMALL_MAX {
        v.extend([p * 5 / 4, p * 6 / 4, p * 7 / 4, p * 2]);
        p *= 2;
    }
    v
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

struct Class {
    size: u64,
    free: Vec<u64>,
    next: u64,
    end: u64,
}

#[derive(Default)]
struct Large {
    by_addr: BTreeMap<u64, u64>,
    by_size: BTreeSet<(u64, u64)>,
    live: HashMap<u64, u64>,
}

impl Large {
    fn insert_free(&mut self, mut addr: u64, mut size: u64) {
        if let Some((&a, &s)) = self.by_addr.range(..addr).next_back()
            && a + s == addr
        {
            self.by_addr.remove(&a);
            self.by_size.remove(&(s, a));
            addr = a;
            size += s;
        }
        if let Some(s) = self.by_addr.remove(&(addr + size)) {
            self.by_size.remove(&(s, addr + size));
            size += s;
        }
        self.by_addr.insert(addr, size);
        self.by_size.insert((size, addr));
    }

    /// Takes `size` bytes at an `align` boundary from the smallest free run that holds them.
    fn take(&mut self, size: u64, align: u64) -> Option<u64> {
        let (s, a, start) = self
            .by_size
            .range((size, 0)..)
            .map(|&(s, a)| (s, a, align_up(a, align)))
            .find(|&(s, a, start)| start + size <= a + s)?;
        self.by_size.remove(&(s, a));
        self.by_addr.remove(&a);
        if start > a {
            self.by_addr.insert(a, start - a);
            self.by_size.insert((start - a, a));
        }
        let tail = a + s - (start + size);
        if tail > 0 {
            self.by_addr.insert(start + size, tail);
            self.by_size.insert((tail, start + size));
        }
        Some(start)
    }
}

/// The heap over `[base, end)`.
pub struct Heap {
    base: u64,
    end: u64,
    sizes: Vec<u64>,
    classes: Vec<Mutex<Class>>,
    /// Per span: 0 for large or free memory, `k + 1` for a span of class `k`.
    spans: Vec<AtomicU8>,
    large: Mutex<Large>,
    used: AtomicU64,
    peak: AtomicU64,
    log: fn(&str),
}

impl Heap {
    /// A heap over `[addr, addr + size)`, shrunk to whole spans. `log` gets a line at every new peak of 256 MiB.
    pub fn new(addr: u64, size: u64, log: fn(&str)) -> Heap {
        let base = align_up(addr, SPAN);
        let end = addr.saturating_add(size) & !(SPAN - 1);
        let end = end.max(base);
        let sizes = class_sizes();
        let classes = sizes
            .iter()
            .map(|&size| {
                Mutex::new(Class {
                    size,
                    free: Vec::new(),
                    next: 0,
                    end: 0,
                })
            })
            .collect();
        let mut large = Large::default();
        if end > base {
            large.insert_free(base, end - base);
        }
        Heap {
            base,
            end,
            sizes,
            classes,
            spans: (0..(end - base) / SPAN).map(|_| AtomicU8::new(0)).collect(),
            large: Mutex::new(large),
            used: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            log,
        }
    }

    /// The range the heap hands out.
    pub fn range(&self) -> (u64, u64) {
        (self.base, self.end)
    }

    /// The bytes in blocks now, and the most there were.
    pub fn in_use(&self) -> (u64, u64) {
        (
            self.used.load(Ordering::Relaxed),
            self.peak.load(Ordering::Relaxed),
        )
    }

    fn norm_align(align: u64) -> u64 {
        align.max(MIN_ALIGN).next_power_of_two()
    }

    /// The class of a small block of `size` bytes at `align`, if one fits.
    fn class_of(&self, size: u64, align: u64) -> Option<usize> {
        if size > SMALL_MAX || align > SMALL_MAX {
            return None;
        }
        let from = self.sizes.partition_point(|&s| s < size);
        self.sizes[from..]
            .iter()
            .position(|s| s.is_multiple_of(align))
            .map(|i| from + i)
    }

    fn large_align(size: u64, align: u64) -> u64 {
        let a = align.max(GRAIN);
        if size >= SPAN { a.max(BIG_ALIGN) } else { a }
    }

    /// What a block of `size` bytes at `align` holds.
    pub fn quantize(&self, size: u64, align: u64) -> u64 {
        let (size, align) = (size.max(1), Self::norm_align(align));
        match self.class_of(size, align) {
            Some(k) => self.sizes[k],
            None => align_up(size, GRAIN),
        }
    }

    fn count(&self, add: u64) {
        let after = self.used.fetch_add(add, Ordering::Relaxed) + add;
        let peak = self.peak.fetch_max(after, Ordering::Relaxed);
        if after > peak && after / PEAK_STEP != peak / PEAK_STEP {
            (self.log)(&format!(
                "[mem] Unreal heap up to {:.2} GiB in use",
                after as f64 / f64::from(1u32 << 30)
            ));
        }
    }

    /// A block of at least `size` bytes at an `align` boundary (0 means 16), or `None` when the range is used up.
    pub fn alloc(&self, size: u64, align: u64) -> Option<u64> {
        let (size, align) = (size.max(1), Self::norm_align(align));
        let p = match self.class_of(size, align) {
            Some(k) => self.small(k)?,
            None => {
                let n = align_up(size, GRAIN);
                let mut l = lock(&self.large);
                let p = l.take(n, Self::large_align(size, align))?;
                l.live.insert(p, n);
                drop(l);
                self.count(n);
                p
            }
        };
        Some(p)
    }

    fn small(&self, k: usize) -> Option<u64> {
        let mut c = lock(&self.classes[k]);
        let size = c.size;
        let p = if let Some(p) = c.free.pop() {
            p
        } else if c.next + size <= c.end {
            let p = c.next;
            c.next += size;
            p
        } else {
            let s = lock(&self.large).take(SPAN, SPAN)?;
            self.spans[((s - self.base) / SPAN) as usize].store(k as u8 + 1, Ordering::Release);
            c.next = s + size;
            c.end = s + SPAN;
            s
        };
        drop(c);
        self.count(size);
        Some(p)
    }

    /// The bytes the block at `p` holds, or `None` when `p` is not a block of this heap.
    pub fn size_of(&self, p: u64) -> Option<u64> {
        if p < self.base || p >= self.end {
            return None;
        }
        let i = ((p - self.base) / SPAN) as usize;
        match self.spans[i].load(Ordering::Acquire) {
            0 => lock(&self.large).live.get(&p).copied(),
            t => {
                let size = self.sizes[usize::from(t - 1)];
                let off = p - (self.base + i as u64 * SPAN);
                (off.is_multiple_of(size) && off + size <= SPAN).then_some(size)
            }
        }
    }

    /// Gives the block at `p` back. Returns false when `p` is not a block of this heap.
    pub fn free(&self, p: u64) -> bool {
        if p < self.base || p >= self.end {
            return false;
        }
        let i = ((p - self.base) / SPAN) as usize;
        let freed = match self.spans[i].load(Ordering::Acquire) {
            0 => {
                let mut l = lock(&self.large);
                match l.live.remove(&p) {
                    Some(n) => {
                        l.insert_free(p, n);
                        Some(n)
                    }
                    None => None,
                }
            }
            t => {
                let mut c = lock(&self.classes[usize::from(t - 1)]);
                let size = c.size;
                let off = p - (self.base + i as u64 * SPAN);
                if off.is_multiple_of(size) && off + size <= SPAN {
                    c.free.push(p);
                    Some(size)
                } else {
                    None
                }
            }
        };
        match freed {
            Some(n) => {
                self.used.fetch_sub(n, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Whether a block of `old` bytes at `p` stays for a resize to `size` bytes at `align`: it is large enough, keeps
    /// the alignment and is not more than twice the size asked for (a small block always stays when it fits).
    pub fn keeps(p: u64, old: u64, size: u64, align: u64) -> bool {
        let align = Self::norm_align(align);
        let need = if size >= SPAN {
            align.max(BIG_ALIGN)
        } else {
            align
        };
        size <= old && p.is_multiple_of(need) && (old <= SMALL_MAX || size * 2 >= old)
    }
}

#[cfg(test)]
mod tests;
