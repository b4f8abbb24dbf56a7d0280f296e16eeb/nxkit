//! The private mirror as the pools see it (`NXKIT_PRIVATE=1`, see "Private GPU memory, step 2" in `docs/nxkit-gpu.md`).
//!
//! An [`Overlay`] is the immutable view of one flush: which 16 KiB pages of guest memory are members (the GPU works on
//! the mirror of them), the runs of the mirror with their buffers, and the pages that the CPU wrote and that the first
//! command buffer of the frame still has to copy into the mirror. [`crate::pools::PoolSnapshot`] holds an optional
//! overlay and asks it first in `resolve_start` and `resolve`, so every site that turns a guest address into a GPU address
//! finds the mirror for a member page, and the guest buffer for the rest.
//!
//! A range resolves to the mirror only when all its pages are members. A range that starts on a member page and reaches
//! past the members is counted ([`partial`]) and resolves to guest memory: the flush makes every binding that touches a
//! member whole, so it should not happen.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nxkit_metal::DataBuffer;

use crate::pools::Backing;

/// The page size of the member bitmap: the host page.
pub(crate) const PAGE: u64 = nxkit_metal::PAGE_SIZE;

static PARTIAL: AtomicU64 = AtomicU64::new(0);

/// Ranges that started on a member page and reached past the members, since the process started.
pub(crate) fn partial() -> u64 {
    PARTIAL.load(Ordering::Relaxed)
}

/// Resolve classes for the bisecting knobs `privnoubo` and the others: a class that is off resolves to guest memory.
pub(crate) const UBO: u8 = 1;
/// Storage buffer bindings.
pub(crate) const SBUF: u8 = 2;
/// Vertex streams, indices, indirect arguments and the pull layout.
pub(crate) const STREAM: u8 = 4;
/// Texture upload sources and texture and buffer copies.
pub(crate) const UPLOAD: u8 = 8;
/// The table of raw global memory.
pub(crate) const GMEM: u8 = 16;
/// Buffer copies and fills.
pub(crate) const COPY: u8 = 32;

/// A run of the mirror: the guest GPU addresses it covers and the buffer over it.
pub struct Run<B> {
    /// The guest address of the first byte.
    pub va_start: u64,
    /// The guest address after the last byte.
    pub va_end: u64,
    /// The buffer. Its real base is the GPU address of `va_start`.
    pub mem: B,
}

/// Pages that the CPU wrote and that the frame's first command buffer copies into the mirror.
pub struct Staged {
    /// The staging buffer.
    pub buf: Arc<DataBuffer>,
    /// `(page, slot)` sorted by page: the bytes of the page are at `slot * PAGE` in the buffer.
    pub slots: Vec<(u32, u32)>,
}

impl Staged {
    fn slot_of(&self, page: u32) -> Option<u32> {
        let i = self.slots.binary_search_by_key(&page, |s| s.0).ok()?;
        Some(self.slots[i].1)
    }
}

/// The members and the mirror runs of one flush.
pub struct Overlay<B: Backing> {
    /// The guest address of the first byte of guest memory.
    va_base: u64,
    /// The member bitmap: bit `i % 64` of word `i / 64` is page `i`.
    bits: Arc<Vec<u64>>,
    /// The runs, sorted by `va_start`.
    runs: Vec<Run<B>>,
    /// The mirror: the mirror byte of the guest address `va` is at the offset `va - va_base`.
    mirror: Option<nxkit_metal::MirrorView>,
    staged: Option<Staged>,
    /// Changes when the members or the runs do.
    generation: u64,
    /// The resolve classes that use guest memory in this frame (the `privno*` knobs).
    off: u8,
}

impl<B: Backing> Overlay<B> {
    /// Makes the view.
    pub fn new(
        va_base: u64,
        bits: Arc<Vec<u64>>,
        runs: Vec<Run<B>>,
        mirror: Option<nxkit_metal::MirrorView>,
        staged: Option<Staged>,
        generation: u64,
    ) -> Self {
        Self {
            va_base,
            bits,
            runs,
            mirror,
            staged,
            generation,
            off: 0,
        }
    }

    /// The same view with the resolve classes in `off` bound to guest memory.
    #[must_use]
    pub fn with_off(mut self, off: u8) -> Self {
        self.off = off;
        self
    }

    /// True when `class` binds guest memory in this frame.
    pub fn off(&self, class: u8) -> bool {
        self.off & class != 0
    }

    /// Changes when the members or the runs do.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The runs.
    pub fn runs(&self) -> &[Run<B>] {
        &self.runs
    }

    /// The staged pages of the frame, if any.
    pub fn staged(&self) -> Option<&Staged> {
        self.staged.as_ref()
    }

    /// The guest address of the first byte of guest memory.
    pub fn va_base(&self) -> u64 {
        self.va_base
    }

    fn page_of(&self, va: u64) -> Option<usize> {
        let off = va.checked_sub(self.va_base)?;
        usize::try_from(off / PAGE).ok()
    }

    fn bit(&self, page: usize) -> bool {
        self.bits
            .get(page / 64)
            .is_some_and(|w| w >> (page % 64) & 1 != 0)
    }

    /// True when the page of `va` is a member.
    pub fn member(&self, va: u64) -> bool {
        self.page_of(va).is_some_and(|p| self.bit(p))
    }

    /// True when every page of `[va, va + len)` is a member. An empty range is the page of `va`.
    pub fn all_members(&self, va: u64, len: u64) -> bool {
        let Some(first) = self.page_of(va) else {
            return false;
        };
        let end = va
            .checked_add(len.max(1))
            .and_then(|e| self.page_of(e - 1))
            .map(|l| l + 1);
        let Some(end) = end else { return false };
        (first..end).all(|p| self.bit(p))
    }

    /// True when a page of `[va, va + len)` is a member.
    pub fn any_member(&self, va: u64, len: u64) -> bool {
        let Some(first) = self.page_of(va) else {
            return false;
        };
        let end = va
            .checked_add(len.max(1))
            .and_then(|e| self.page_of(e - 1))
            .map(|l| l + 1);
        end.is_some_and(|end| (first..end).any(|p| self.bit(p)))
    }

    /// The member runs inside `[lo, hi)`: sorted `(start, end)` of guest addresses, whole pages clipped to the range.
    pub fn member_runs_in(&self, lo: u64, hi: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        if hi <= lo {
            return out;
        }
        let (Some(first), Some(last)) = (self.page_of(lo), self.page_of(hi - 1)) else {
            return out;
        };
        let mut start: Option<usize> = None;
        for p in first..=last {
            match (self.bit(p), start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    out.push(self.clip(s, p, lo, hi));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            out.push(self.clip(s, last + 1, lo, hi));
        }
        out
    }

    fn clip(&self, s: usize, e: usize, lo: u64, hi: u64) -> (u64, u64) {
        (
            (self.va_base + s as u64 * PAGE).max(lo),
            (self.va_base + e as u64 * PAGE).min(hi),
        )
    }

    /// Where the mirror holds the first byte of the member page of `va`: the index of its run, the real address and the
    /// bytes from `va` to the end of the run. `None` when the page is no member or lies in no run.
    pub fn locate(&self, va: u64) -> Option<(usize, u64, u64)> {
        if !self.member(va) {
            return None;
        }
        let i = self
            .runs
            .partition_point(|r| r.va_start <= va)
            .checked_sub(1)?;
        let r = &self.runs[i];
        (va < r.va_end).then(|| (i, r.mem.real_base() + (va - r.va_start), r.va_end - va))
    }

    /// Reads `out.len()` bytes of the frame's view of guest memory at `va`: staged bytes for a page that the CPU wrote
    /// before the flush, the mirror for the other member pages, and `other` (called with the guest address and the part of
    /// `out`) for the pages that are no members. Returns false, and leaves `out` unspecified, when a part cannot be read.
    pub fn read_with(
        &self,
        va: u64,
        out: &mut [u8],
        other: &mut dyn FnMut(u64, &mut [u8]) -> bool,
    ) -> bool {
        let mut done = 0usize;
        while done < out.len() {
            let at = va + done as u64;
            let Some(page) = self.page_of(at) else {
                return other(at, &mut out[done..]);
            };
            let in_page = (PAGE - (at - self.va_base) % PAGE) as usize;
            let n = in_page.min(out.len() - done);
            let off = ((at - self.va_base) % PAGE) as usize;
            if !self.bit(page) {
                if !other(at, &mut out[done..done + n]) {
                    return false;
                }
                done += n;
                continue;
            }
            let Some(mirror) = &self.mirror else {
                return false;
            };
            let slot = self
                .staged
                .as_ref()
                .and_then(|s| s.slot_of(page as u32).map(|k| (s, k)));
            let ok = match slot {
                Some((s, k)) => s
                    .buf
                    .read(u64::from(k) * PAGE + off as u64, &mut out[done..done + n])
                    .is_ok(),
                None => mirror
                    .read(at - self.va_base, &mut out[done..done + n])
                    .is_ok(),
            };
            if !ok {
                return false;
            }
            done += n;
        }
        true
    }

    /// [`read_with`](Self::read_with) for a range of member pages only: false when a page is no member.
    pub fn read(&self, va: u64, out: &mut [u8]) -> bool {
        self.read_with(va, out, &mut |_, _| false)
    }
}

/// Counts a range that started on a member page and reached past the members.
pub(crate) fn note_partial() {
    PARTIAL.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Fake(u64);

    impl Backing for Fake {
        fn real_base(&self) -> u64 {
            self.0
        }
    }

    const BASE: u64 = 0xc0_0000_0000;

    fn overlay(members: &[usize]) -> Overlay<Fake> {
        let mut bits = vec![0u64; 4];
        for &p in members {
            bits[p / 64] |= 1 << (p % 64);
        }
        let runs = vec![Run {
            va_start: BASE + 2 * PAGE,
            va_end: BASE + 10 * PAGE,
            mem: Fake(0x9000_0000),
        }];
        Overlay::new(BASE, Arc::new(bits), runs, None, None, 1)
    }

    #[test]
    fn membership_is_by_page_and_a_range_needs_all_its_pages() {
        let o = overlay(&[2, 3, 4, 7]);
        assert!(o.member(BASE + 2 * PAGE));
        assert!(o.member(BASE + 3 * PAGE + 5));
        assert!(!o.member(BASE + 5 * PAGE));
        assert!(!o.member(BASE - 1));
        assert!(o.all_members(BASE + 2 * PAGE, 3 * PAGE));
        assert!(!o.all_members(BASE + 2 * PAGE, 3 * PAGE + 1));
        assert!(o.all_members(BASE + 4 * PAGE + 100, 1));
        assert!(!o.all_members(BASE + 6 * PAGE, 0));
    }

    #[test]
    fn locate_gives_the_run_the_real_address_and_the_room() {
        let o = overlay(&[2, 3, 4, 7]);
        let (i, real, room) = o.locate(BASE + 3 * PAGE + 16).unwrap();
        assert_eq!(i, 0);
        assert_eq!(real, 0x9000_0000 + PAGE + 16);
        assert_eq!(room, 7 * PAGE - 16);
        assert!(o.locate(BASE + 5 * PAGE).is_none(), "not a member");
        let o = overlay(&[12]);
        assert!(
            o.locate(BASE + 12 * PAGE).is_none(),
            "a member outside every run"
        );
    }

    #[test]
    fn member_runs_are_clipped_to_the_range() {
        let o = overlay(&[2, 3, 4, 7]);
        let r = o.member_runs_in(BASE + PAGE, BASE + 9 * PAGE);
        assert_eq!(
            r,
            vec![
                (BASE + 2 * PAGE, BASE + 5 * PAGE),
                (BASE + 7 * PAGE, BASE + 8 * PAGE)
            ]
        );
        let r = o.member_runs_in(BASE + 3 * PAGE + 4, BASE + 4 * PAGE + 4);
        assert_eq!(r, vec![(BASE + 3 * PAGE + 4, BASE + 4 * PAGE + 4)]);
        assert!(
            o.member_runs_in(BASE + 5 * PAGE, BASE + 7 * PAGE)
                .is_empty()
        );
    }

    #[test]
    fn a_read_takes_the_mirror_bytes_and_the_staged_bytes_of_a_written_page() {
        let Ok(ctx) = nxkit_metal::Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let mut mirror = nxkit_metal::Mirror::new(&ctx, 2 * nxkit_metal::MIRROR_SEGMENT).unwrap();
        mirror.ensure(&[(0, 1)]).unwrap();
        let view = mirror.view();
        let bytes: Vec<u8> = (0..4 * PAGE as usize).map(|i| (i % 251) as u8).collect();
        view.write(0, &bytes).unwrap();
        let mut o = Overlay::new(
            BASE,
            Arc::new(vec![0b1111u64]),
            Vec::<Run<Fake>>::new(),
            Some(view),
            None,
            1,
        );
        let mut out = [0u8; 8];
        assert!(o.read(BASE + PAGE - 4, &mut out), "across a page border");
        let at = (PAGE - 4) as usize;
        assert_eq!(&out[..], &bytes[at..at + 8]);
        assert!(!o.read(BASE + 4 * PAGE, &mut out), "past the members");
        // A staged page reads from the staging buffer.
        let buf = DataBuffer::new(&ctx, 2 * PAGE).unwrap();
        buf.write(PAGE, &[0xee; 16]).unwrap();
        o.staged = Some(Staged {
            buf: Arc::new(buf),
            slots: vec![(2, 1)],
        });
        let mut out = [0u8; 8];
        assert!(o.read(BASE + 2 * PAGE + 4, &mut out));
        assert_eq!(out, [0xee; 8]);
        assert!(o.read(BASE + PAGE, &mut out));
        assert_eq!(&out[..], &bytes[PAGE as usize..PAGE as usize + 8]);
    }
}
