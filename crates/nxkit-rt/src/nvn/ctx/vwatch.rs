//! The watch on freshly mapped pages of sparse pools.
//!
//! Unreal Engine 4.18 (Octopath Traveler) streams the higher mips of a texture this way: it maps pages of a physical
//! pool into the texture's virtual pool, then copies the mip data into those physical pages with the CPU. The pool is
//! not CPU cached, so the game calls no `nvnMemoryPoolFlushMappedRange`, and no command copies the data. On the console
//! the GPU reads the pages directly. The host keeps its own copy of the texture, which it loads at the mapping request
//! (when the pages still hold zeros) and does not load again. The texture then samples black in those mips.
//!
//! So each mapping request starts a watch on its physical pages. At every flush the watch hashes them, and a change
//! makes the textures over the virtual range load again. A watch ends when its pages kept the same non-zero content for
//! [`QUIET_FLUSHES`] flushes, or after [`MAX_FLUSHES`] flushes, when they are unmapped, or when the range is mapped again.

use nxkit_abi::guest;
use nxkit_nvn::space::GpuAddr;

/// The flushes without a change after which a watch on non-zero pages ends.
pub(super) const QUIET_FLUSHES: u32 = 120;
/// The flushes after which a watch ends in any case.
pub(super) const MAX_FLUSHES: u32 = 3600;

/// One watched mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Watch {
    /// The virtual range that the request changed.
    virt: (GpuAddr, u64),
    /// The guest host address of the physical pages.
    host: u64,
    /// The hash of the pages at the last flush, None while they are all zero.
    hash: Option<u64>,
    /// Flushes since the last change.
    quiet: u32,
    /// Flushes since the request.
    age: u32,
}

/// The watches of the context.
#[derive(Debug, Default)]
pub(super) struct Watches {
    list: Vec<Watch>,
}

impl Watches {
    /// A mapping request mapped `virt` onto the physical pages at `host`, or unmapped it when `host` is None. A watch
    /// that overlaps the range ends, and a mapping starts one.
    pub(super) fn mapped(&mut self, virt: (GpuAddr, u64), host: Option<u64>) {
        let (start, len) = (virt.0.get(), virt.1);
        self.list.retain(|w| {
            let (s, l) = (w.virt.0.get(), w.virt.1);
            s.saturating_add(l) <= start || start.saturating_add(len) <= s
        });
        if let Some(host) = host {
            self.list.push(Watch {
                virt,
                host,
                hash: content(host, len),
                quiet: 0,
                age: 0,
            });
        }
    }

    /// Ends the watches inside `[start, start + len)` of virtual addresses (a pool that went away).
    pub(super) fn released(&mut self, start: u64, len: u64) {
        self.list.retain(|w| {
            let s = w.virt.0.get();
            s < start || s >= start.saturating_add(len)
        });
    }

    /// One flush: the virtual ranges whose pages changed since the last flush go to `changed`.
    pub(super) fn step(&mut self, changed: &mut Vec<(GpuAddr, u64)>) {
        self.list.retain_mut(|w| {
            let now = content(w.host, w.virt.1);
            w.age += 1;
            if now == w.hash {
                w.quiet += 1;
            } else {
                w.hash = now;
                w.quiet = 0;
                changed.push(w.virt);
            }
            let settled = w.hash.is_some() && w.quiet >= QUIET_FLUSHES;
            !settled && w.age < MAX_FLUSHES
        });
    }
}

/// A hash of the guest bytes `[host, host + len)`, None when they are all zero or not readable.
fn content(host: u64, len: u64) -> Option<u64> {
    guest::with_slice(host, len, |b| {
        let (words, rest) = b.as_chunks::<8>();
        let mut h: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut any = 0u64;
        for w in words
            .iter()
            .map(|w| u64::from_le_bytes(*w))
            .chain(rest.iter().map(|&x| u64::from(x)))
        {
            any |= w;
            h = (h ^ w).wrapping_mul(0x0100_0000_01B3).rotate_left(29);
        }
        (any != 0).then_some(h)
    })
    .ok()
    .flatten()
}
