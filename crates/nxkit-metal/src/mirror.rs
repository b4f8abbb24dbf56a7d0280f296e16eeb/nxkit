//! The GPU's private copy of guest memory (`NXKIT_PRIVATE=1`, see "Private GPU memory, step 2: the mirror" in
//! `docs/nxkit-gpu.md`).
//!
//! The mirror is a host reservation as large as guest memory (an anonymous mapping without access, so it costs no
//! physical memory). Byte `o` of guest memory has its mirror byte at `host + o`. The reservation is cut into segments of
//! [`SEGMENT`] (2 MiB). A segment becomes usable (read and write) when it holds a page that the GPU works on, and stays
//! usable. A maximal run of adjacent usable segments has one no-copy `MTLBuffer` over it ([`GuestMemory`]), so the GPU
//! address of a mirror byte is the `real_base` of its run plus the offset in the run. When a run grows, or a segment
//! joins two runs, a new buffer is made over the larger range of the same host memory; the bytes do not move. The old
//! buffer stays in the residency set until nothing else holds it (a frame in flight, an indirect command buffer), and
//! is dropped by [`Mirror::ensure`] on the calling thread, never on a completion handler.
//!
//! Nothing here knows which pages are members; the caller says which ranges need to be usable.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::guest::GuestMemory;

/// The bytes of a segment.
pub const SEGMENT: u64 = 2 * 1024 * 1024;

const PROT_NONE: i32 = 0;
const PROT_RW: i32 = 3;
const MAP_PRIVATE: i32 = 0x2;
const MAP_ANON: i32 = 0x1000;
const MAP_NORESERVE: i32 = 0x40;

unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
    -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
}

/// A run of usable segments with its buffer.
#[derive(Clone)]
pub struct MirrorRun {
    /// The offset of the first byte from the start of guest memory.
    pub start: u64,
    /// The offset after the last byte.
    pub end: u64,
    /// The no-copy buffer over `[host + start, host + end)`. Its `real_base` is the GPU address of the first byte.
    pub mem: Arc<GuestMemory>,
}

/// What [`Mirror::ensure`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ensured {
    /// Segments that became usable.
    pub new_segments: u32,
    /// Buffers made (a run that grew counts as one).
    pub new_buffers: u32,
    /// Buffers that left the set of runs and wait for the frames that hold them.
    pub retired: u32,
}

/// A handle for reading the mirror. It is valid while the mirror lives (the host keeps it for the whole run).
#[derive(Clone)]
pub struct MirrorView {
    host: u64,
    usable: Arc<Vec<AtomicBool>>,
}

impl MirrorView {
    /// Copies `out.len()` bytes of the mirror at the offset `offset` (from the start of guest memory). The segments
    /// that hold the range must be usable. The GPU may write the bytes at the same time: the caller orders that.
    pub fn read(&self, offset: u64, out: &mut [u8]) -> Result<()> {
        if out.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(out.len() as u64)
            .ok_or(MetalError::OutOfRange {
                addr: offset,
                len: out.len() as u64,
            })?;
        let ok = (offset / SEGMENT..end.div_ceil(SEGMENT)).all(|s| {
            self.usable
                .get(s as usize)
                .is_some_and(|u| u.load(Ordering::Acquire))
        });
        if !ok {
            return Err(MetalError::OutOfRange {
                addr: offset,
                len: out.len() as u64,
            });
        }
        // SAFETY: the segments that hold the range are usable (read and write), so the bytes are mapped; they stay mapped
        // while the mirror lives, which the host keeps for the whole run; `out` is a separate allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(
                (self.host + offset) as *const u8,
                out.as_mut_ptr(),
                out.len(),
            );
        }
        Ok(())
    }
}

impl MirrorView {
    /// Copies `data` into the mirror at the offset `offset`, under the conditions of [`MirrorView::read`]. For tests and
    /// for callers that fill the mirror with the CPU.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(MetalError::OutOfRange {
                addr: offset,
                len: data.len() as u64,
            })?;
        let ok = (offset / SEGMENT..end.div_ceil(SEGMENT)).all(|s| {
            self.usable
                .get(s as usize)
                .is_some_and(|u| u.load(Ordering::Acquire))
        });
        if !ok {
            return Err(MetalError::OutOfRange {
                addr: offset,
                len: data.len() as u64,
            });
        }
        // SAFETY: as for `read`; `data` is a separate allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (self.host + offset) as *mut u8,
                data.len(),
            );
        }
        Ok(())
    }
}

/// The mirror of a guest memory region.
pub struct Mirror {
    ctx: Context,
    host: NonNull<u8>,
    /// Bytes reserved: the guest length rounded up to a segment.
    len: u64,
    usable: Arc<Vec<AtomicBool>>,
    runs: Vec<MirrorRun>,
    /// Buffers that left `runs`; dropped here once nothing else holds them.
    retired: Vec<Arc<GuestMemory>>,
    /// Counts changes of `runs`.
    generation: u64,
}

// SAFETY: the pointer names a mapping that lives as long as the mirror; the mirror hands out the address only, and
// the bytes behind it are the callers' to order.
unsafe impl Send for Mirror {}

impl Mirror {
    /// Reserves the mirror of `guest_len` bytes. No page is usable yet.
    pub fn new(ctx: &Context, guest_len: u64) -> Result<Self> {
        let len = guest_len.div_ceil(SEGMENT) * SEGMENT;
        let size = usize::try_from(len)
            .map_err(|_| MetalError::Invalid("mirror length does not fit usize"))?;
        if size == 0 {
            return Err(MetalError::Invalid("empty mirror"));
        }
        // SAFETY: an anonymous private mapping with no access; nothing is read or written through it here.
        let p = unsafe {
            mmap(
                std::ptr::null_mut(),
                size,
                PROT_NONE,
                MAP_PRIVATE | MAP_ANON | MAP_NORESERVE,
                -1,
                0,
            )
        };
        if p as isize == -1 {
            return Err(create("mirror", "mmap of the reservation failed"));
        }
        let host =
            NonNull::new(p.cast::<u8>()).ok_or_else(|| create("mirror", "mmap gave null"))?;
        Ok(Self {
            ctx: ctx.clone(),
            host,
            len,
            usable: Arc::new((0..len / SEGMENT).map(|_| AtomicBool::new(false)).collect()),
            runs: Vec::new(),
            retired: Vec::new(),
            generation: 0,
        })
    }

    /// The host address of the first byte of the mirror.
    pub fn host_base(&self) -> u64 {
        self.host.as_ptr() as u64
    }

    /// The bytes reserved.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Always false.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// A handle that reads the mirror from any thread.
    pub fn view(&self) -> MirrorView {
        MirrorView {
            host: self.host.as_ptr() as u64,
            usable: self.usable.clone(),
        }
    }

    /// The runs, sorted by start.
    pub fn runs(&self) -> &[MirrorRun] {
        &self.runs
    }

    /// Counts the changes of the runs.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The bytes of the usable segments.
    pub fn usable_bytes(&self) -> u64 {
        self.usable
            .iter()
            .filter(|u| u.load(Ordering::Acquire))
            .count() as u64
            * SEGMENT
    }

    /// Buffers that wait to be dropped.
    pub fn retired(&self) -> usize {
        self.retired.len()
    }

    /// Makes the segments that hold the byte ranges `(start, end)` (offsets from the start of guest memory) usable, and
    /// the runs and their buffers follow. A range beyond the reservation is an error. The residency set is not
    /// committed: the encoder commits it before the frame that uses a new buffer is submitted.
    pub fn ensure(&mut self, ranges: &[(u64, u64)]) -> Result<Ensured> {
        let mut out = Ensured::default();
        // The buffers that nothing holds any more.
        let before = self.retired.len();
        self.retired.retain(|m| Arc::strong_count(m) > 1);
        let dropped = before - self.retired.len();
        if dropped > 0 {
            self.ctx.commit_residency();
        }
        for &(s, e) in ranges {
            if e <= s {
                continue;
            }
            if e > self.len {
                return Err(MetalError::OutOfRange {
                    addr: s,
                    len: e - s,
                });
            }
            for seg in (s / SEGMENT)..e.div_ceil(SEGMENT) {
                let seg = seg as usize;
                if self.usable[seg].load(Ordering::Acquire) {
                    continue;
                }
                let at = self.host_base() + seg as u64 * SEGMENT;
                // SAFETY: the segment lies inside the reservation.
                if unsafe { mprotect(at as *mut c_void, SEGMENT as usize, PROT_RW) } != 0 {
                    return Err(create("mirror", "mprotect of a segment failed"));
                }
                self.usable[seg].store(true, Ordering::Release);
                out.new_segments += 1;
            }
        }
        if out.new_segments > 0 {
            self.remake_runs(&mut out)?;
        }
        Ok(out)
    }

    fn remake_runs(&mut self, out: &mut Ensured) -> Result<()> {
        // The maximal runs of usable segments.
        let mut want: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        let on = |i: usize| self.usable[i].load(Ordering::Acquire);
        while i < self.usable.len() {
            if on(i) {
                let s = i;
                while i < self.usable.len() && on(i) {
                    i += 1;
                }
                want.push((s, i));
            } else {
                i += 1;
            }
        }
        let mut runs: Vec<MirrorRun> = Vec::with_capacity(want.len());
        for (s, e) in want {
            let (start, end) = (s as u64 * SEGMENT, e as u64 * SEGMENT);
            if let Some(r) = self.runs.iter().find(|r| r.start == start && r.end == end) {
                runs.push(r.clone());
                continue;
            }
            let at = NonNull::new((self.host_base() + start) as *mut u8)
                .ok_or(MetalError::Invalid("mirror run at null"))?;
            // SAFETY: the range lies in the reservation, whose segments are usable (read and write) and stay mapped for
            // the life of the mirror, which drops the buffer first; `start` and the length are multiples of the page.
            let mem = unsafe { GuestMemory::from_raw_uncommitted(&self.ctx, at, end - start) }?;
            out.new_buffers += 1;
            runs.push(MirrorRun {
                start,
                end,
                mem: Arc::new(mem),
            });
        }
        // The runs that are gone are retired, not dropped: a frame in flight may still use them.
        for old in std::mem::take(&mut self.runs) {
            if !runs.iter().any(|r| Arc::ptr_eq(&r.mem, &old.mem)) {
                out.retired += 1;
                self.retired.push(old.mem);
            }
        }
        self.runs = runs;
        self.generation += 1;
        Ok(())
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        self.runs.clear();
        self.retired.clear();
        self.ctx.commit_residency();
        // SAFETY: the mapping is the one `new` made; the buffers over it are gone.
        unsafe { munmap(self.host.as_ptr().cast(), self.len as usize) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Option<Context> {
        match Context::system_default() {
            Ok(c) => Some(c),
            Err(_) => {
                eprintln!("skipped: no Metal device");
                None
            }
        }
    }

    #[test]
    fn segments_become_usable_and_adjacent_ones_share_a_buffer() {
        let Some(ctx) = ctx() else { return };
        let mut m = Mirror::new(&ctx, 64 * SEGMENT).unwrap();
        assert!(m.runs().is_empty());
        let e = m.ensure(&[(SEGMENT + 5, SEGMENT + 9)]).unwrap();
        assert_eq!((e.new_segments, e.new_buffers), (1, 1));
        assert_eq!(m.runs().len(), 1);
        assert_eq!((m.runs()[0].start, m.runs()[0].end), (SEGMENT, 2 * SEGMENT));
        // The segment is writable.
        // SAFETY: inside the usable segment.
        unsafe {
            (m.host_base() as *mut u8)
                .add(SEGMENT as usize + 7)
                .write(9)
        };
        // The same segment again: nothing changes.
        let e = m.ensure(&[(SEGMENT, SEGMENT + 1)]).unwrap();
        assert_eq!(e, Ensured::default());
        // An adjacent segment grows the run: a new buffer over the larger range, the old one retires.
        let old = m.runs()[0].mem.clone();
        let e = m.ensure(&[(2 * SEGMENT, 2 * SEGMENT + 1)]).unwrap();
        assert_eq!((e.new_segments, e.new_buffers, e.retired), (1, 1, 1));
        assert_eq!(m.runs().len(), 1);
        assert_eq!((m.runs()[0].start, m.runs()[0].end), (SEGMENT, 3 * SEGMENT));
        // SAFETY: inside the usable segment.
        let seen = unsafe {
            (m.host_base() as *const u8)
                .add(SEGMENT as usize + 7)
                .read()
        };
        assert_eq!(seen, 9);
        // A far segment is a second run; filling the gap joins all.
        m.ensure(&[(10 * SEGMENT, 11 * SEGMENT)]).unwrap();
        assert_eq!(m.runs().len(), 2);
        let e = m.ensure(&[(3 * SEGMENT, 10 * SEGMENT)]).unwrap();
        assert_eq!(e.new_segments, 7);
        assert_eq!(m.runs().len(), 1);
        assert_eq!(
            (m.runs()[0].start, m.runs()[0].end),
            (SEGMENT, 11 * SEGMENT)
        );
        // A buffer that is held outside stays retired; one that is not is dropped at the next call.
        assert!(m.retired() >= 1);
        drop(old);
        m.ensure(&[]).unwrap();
        assert!(m.ensure(&[(0, 65 * SEGMENT)]).is_err());
    }
}
