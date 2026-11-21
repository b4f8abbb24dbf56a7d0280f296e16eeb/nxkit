//! Attaching the tracker, its switches and the queries of its state.

use super::*;

impl Pages {
    /// The host address of the first tracked page (the guest base rounded up to a page).
    pub fn host_base(&self) -> u64 {
        SHARED.base.load(Ordering::Acquire)
    }

    /// The page numbers (first, end) of the host range, for callers that keep their own tables.
    pub fn page_range(&self, addr: u64, len: u64) -> Option<(u32, u32)> {
        page_span(addr, len)
    }

    /// The tracker.
    pub fn global() -> &'static Pages {
        &PAGES
    }

    /// Names the guest range `[base, base + len)`, makes the table and installs the signal handler and the hook of
    /// `nxkit_abi::guest`. The host calls it once, after the crash report is installed, because the handler hands the
    /// faults it does not own to the handler it finds.
    pub fn attach(&self, base: u64, len: u64) {
        eager_quiesce();
        let p = page();
        let (b, e) = (base.div_ceil(p) * p, (base + len) & !(p - 1));
        let len = e.saturating_sub(b);
        let n = (len / p) as usize;
        // A table of zero words: the system gives zero pages that cost nothing until a page is tracked.
        let Ok(layout) = std::alloc::Layout::array::<AtomicU32>(n.max(1)) else {
            return;
        };
        // SAFETY: the layout is not empty; zeroed memory is a valid table of free pages. The table is never freed.
        let table = unsafe { std::alloc::alloc_zeroed(layout) }.cast::<AtomicU32>();
        if table.is_null() {
            return;
        }
        let Ok(bits_layout) = std::alloc::Layout::array::<AtomicU64>(n.div_ceil(64).max(1)) else {
            return;
        };
        // SAFETY: as for the table: a non-empty layout, zeroed memory is a valid bitmap, and it is never freed.
        let bits = unsafe { std::alloc::alloc_zeroed(bits_layout) }.cast::<AtomicU64>();
        if bits.is_null() {
            return;
        }
        let Ok(info_layout) = std::alloc::Layout::array::<PageInfo>(n.max(1)) else {
            return;
        };
        // SAFETY: as for the table: zero bytes are a valid `PageInfo`, and the array is never freed.
        let infos = unsafe { std::alloc::alloc_zeroed(info_layout) }.cast::<PageInfo>();
        if infos.is_null() {
            return;
        }
        SHARED.alias.store(0, Ordering::Release);
        SHARED.info.store(infos, Ordering::Release);
        SHARED.members.store(0, Ordering::Release);
        SHARED.completed.store(0, Ordering::Release);
        SHARED.flushed.store(0, Ordering::Release);
        SHARED.committed.store(0, Ordering::Release);
        SHARED.mirror.store(0, Ordering::Release);
        SHARED.npages.store(0, Ordering::Release);
        SHARED.member_bits.store(bits, Ordering::Release);
        SHARED.member_gen.fetch_add(1, Ordering::AcqRel);
        SHARED.table.store(table, Ordering::Release);
        SHARED.base.store(b, Ordering::Release);
        SHARED.len.store(len, Ordering::Release);
        SHARED.npages.store(n, Ordering::Release);
        SHARED.log_n.store(0, Ordering::Release);
        SHARED.readers_n.store(0, Ordering::Release);
        {
            let mut l = LEARNED.lock().unwrap_or_else(PoisonError::into_inner);
            l.pages.clear();
        }
        {
            let mut c = ctrl();
            c.frames.clear();
            c.masks.clear();
            c.late_copies.clear();
            c.carry.clear();
            c.snap = None;
            c.streak = vec![0; n];
            c.prev_dirty.clear();
            c.hot.clear();
        }
        install_handler();
        nxkit_abi::guest::set_guard_hook(hook);
    }

    /// Maps guest memory a second time with read and write access, for the emulation of accesses outside the blocks a frame
    /// in flight writes (step 4). It needs the guest range to be readable and writable, so the host calls it before the
    /// pages are protected, and with the mirror. Returns an error text when it could not (the handler then waits, as in
    /// step 3). A host page of more than 16 KiB cannot be covered by the four-word mask.
    pub fn make_alias(&self) -> Result<(), String> {
        if !self.attached() {
            return Err("the tracker is not attached".to_string());
        }
        if page() > 4 * 64 * BLOCK {
            return Err("the host page has more blocks than the mask holds".to_string());
        }
        let (base, len) = (
            SHARED.base.load(Ordering::Acquire),
            SHARED.len.load(Ordering::Acquire),
        );
        let alias = build_alias(base, len)?;
        SHARED.alias.store(alias, Ordering::Release);
        Ok(())
    }

    /// Turns the emulation on or off (on by default once the alias exists); off, every access to a page in flight waits.
    pub fn set_emulation(&self, on: bool) {
        SHARED.emulate.store(on, Ordering::Release);
    }

    /// True when the handler can emulate accesses (the alias exists and the emulation is on).
    pub fn emulating(&self) -> bool {
        SHARED.alias.load(Ordering::Acquire) != 0 && SHARED.emulate.load(Ordering::Acquire)
    }

    /// The bytes of the guest page `page` through the alias, whatever its protection; None without an alias.
    pub(super) fn alias_page(&self, pg: u32) -> Option<&'static [u8]> {
        let alias = SHARED.alias.load(Ordering::Acquire);
        if alias == 0 {
            return None;
        }
        let p = page();
        // SAFETY: the alias maps the whole guest range with read access and `pg` is a page inside the table; the bytes
        // may change under a reader (the CPU stores through it), which the staging accepts.
        Some(unsafe {
            std::slice::from_raw_parts((alias + u64::from(pg) * p) as *const u8, p as usize)
        })
    }

    /// Whether [`attach`](Self::attach) ran.
    pub fn attached(&self) -> bool {
        SHARED.npages.load(Ordering::Acquire) != 0
    }

    /// The host page size.
    pub fn page_size(&self) -> u64 {
        page()
    }

    /// The state of the page that holds the host address: `free`, `clean`, `dirty` or `gpu(frame)` as text, for tests
    /// and reports.
    pub fn state_of(&self, host_addr: u64) -> String {
        let base = SHARED.base.load(Ordering::Acquire);
        let Some(w) = host_addr
            .checked_sub(base)
            .and_then(|o| word((o / page()) as usize))
        else {
            return "outside".to_string();
        };
        let v = w.load(Ordering::Acquire);
        match v & STATE_MASK {
            FREE => "free".to_string(),
            CLEAN => "clean".to_string(),
            DIRTY => "dirty".to_string(),
            SYNC => "sync".to_string(),
            OPEN => format!("open({})", v >> FRAME_SHIFT),
            _ if v & FRESH != 0 => format!("gpu({}, fresh)", v >> FRAME_SHIFT),
            _ => format!("gpu({})", v >> FRAME_SHIFT),
        }
    }

    /// Declares the mirror: the host address of a reservation as large as guest memory, whose byte `o` is the GPU's copy
    /// of guest byte `o`. From now on a page tagged with a frame holds stale guest bytes, and a CPU access to it waits
    /// for the frame and copies the page from the mirror ([`flush_mirrored`](Self::flush_mirrored)). 0 turns it off.
    pub fn set_mirror(&self, host_base: u64) {
        SHARED.mirror.store(host_base, Ordering::Release);
    }

    /// Step 5, off by default: `eager` copies back, after a frame completes and on a thread of its own, the pages that game
    /// threads faulted on lately for a copy from the mirror (it needs the alias); `hot` keeps the pages that the CPU
    /// dirties in many flushes in a row writable and diffs them at each flush instead of faulting on them.
    pub fn set_adaptive(&self, eager: bool, hot: bool) {
        SHARED.eager_on.store(eager, Ordering::Release);
        SHARED.hot_on.store(hot, Ordering::Release);
    }

    /// Results one frame late (needs the alias and the emulation): a game thread's access to the blocks that a frame in
    /// flight writes is served from guest memory, which holds the last completed frame's bytes there, without a wait.
    /// A page qualifies after a game thread has waited for it once. A store goes to guest memory too and wins over the
    /// copy-back of the frame in flight; the next flush stages it into the mirror.
    pub fn set_late(&self, on: bool) {
        SHARED.late.store(on, Ordering::Release);
    }

    /// Results one frame late: the pages of the flushed frame `frame` that game threads read late, sorted. The encoder
    /// has the GPU copy each of them from the mirror twice, into consecutive pages of a buffer: after the copy-in at
    /// the start of the frame and after its last pass, and registers the two copies with
    /// [`set_late_copies`](Self::set_late_copies). Empty when there are none.
    pub fn late_copy_pages(&self, frame: u64) -> Vec<u32> {
        let c = ctrl();
        c.late_copies
            .iter()
            .rev()
            .find(|l| l.frame == frame)
            .map(|l| l.pages.clone())
            .unwrap_or_default()
    }

    /// The copies of [`late_copy_pages`](Self::late_copy_pages) of frame `frame` are encoded: page `i` of the list is
    /// at `start + i * page` and `end + i * page` (host addresses of shared memory that stays valid until
    /// [`settled`](Self::settled) reaches `frame`). Call it before the frame is committed. Without it, the completion
    /// of the frame copies from the mirror where that is exact and drops the snapshot of the other pages.
    pub fn set_late_copies(&self, frame: u64, start: u64, end: u64) {
        let mut c = ctrl();
        if let Some(l) = c.late_copies.iter_mut().rev().find(|l| l.frame == frame) {
            l.start = start;
            l.end = end;
        }
    }

    /// The newest frame whose completion has finished with its copies (the staging buffers and the GPU's copies of the
    /// frames up to it may be used again).
    pub fn settled(&self) -> u64 {
        SHARED.settled.load(Ordering::Acquire)
    }

    /// Whether the flush leaves the pages with a snapshot readable while a frame in flight writes them (needs
    /// [`set_late`](Self::set_late)): a read of such a page takes no trap and gets the last completed frame's bytes.
    pub fn set_read_open(&self, on: bool) {
        SHARED.read_open_on.store(on, Ordering::Release);
    }

    /// True when results come one frame late ([`set_late`](Self::set_late)).
    pub fn late(&self) -> bool {
        SHARED.late.load(Ordering::Acquire)
    }

    /// The largest age in frames of the bytes served late since the last call.
    pub fn take_late_age_max(&self) -> u64 {
        SHARED.late_age_max.swap(0, Ordering::Relaxed)
    }

    /// Stops tracking every page and gives them all read and write access again: for a host that lets the range go, and for
    /// tests whose guest memory goes back to the allocator.
    pub fn release_all(&self) {
        if self.attached() {
            self.forget(
                SHARED.base.load(Ordering::Acquire),
                SHARED.len.load(Ordering::Acquire),
            );
        }
    }

    /// Drops the queued eager copies and waits for the one that runs. The host calls it before it unmaps the guest range
    /// (the copy writes through the alias); `attach` does it too.
    pub fn quiesce(&self) {
        eager_quiesce();
    }

    /// The first command buffer of `frame` was committed: the writes of the CPU from here on are no longer "between the
    /// flush and the commit" of that frame.
    pub fn note_committed(&self, frame: u64) {
        SHARED.committed.store(frame, Ordering::Release);
    }

    /// Copies `buf.len()` bytes of guest memory at the host address `addr` into `buf`, when every page of the range is a
    /// member that is readable now (clean or dirty, not tagged for a frame). Returns false otherwise. For the
    /// diagnostics: the pages may change state right after the check.
    pub fn peek(&self, addr: u64, buf: &mut [u8]) -> bool {
        let base = SHARED.base.load(Ordering::Acquire);
        let total = SHARED.len.load(Ordering::Acquire);
        if buf.is_empty()
            || addr < base
            || addr.saturating_add(buf.len() as u64) > base.saturating_add(total)
        {
            return false;
        }
        let Some((first, end)) = page_span(addr, buf.len() as u64) else {
            return false;
        };
        let readable = (first..end).all(|p| {
            word(p as usize)
                .is_some_and(|w| matches!(w.load(Ordering::Acquire) & STATE_MASK, CLEAN | DIRTY))
        });
        if !readable {
            return false;
        }
        // SAFETY: the range lies in the guest memory that `attach` named (checked above) and every page of it is a member
        // that is clean (read only) or dirty (read and write), so the read cannot fault.
        unsafe {
            std::ptr::copy_nonoverlapping(addr as *const u8, buf.as_mut_ptr(), buf.len());
        }
        true
    }

    /// The first accesses of game threads to pages of a frame in flight since the last call, as `(frame, host address,
    /// write)` (at most 1024; the second number is how many there were).
    pub fn take_in_flight(&self) -> (Vec<(u32, u64, bool)>, u32) {
        let base = SHARED.base.load(Ordering::Acquire);
        let n = SHARED.flight_n.swap(0, Ordering::AcqRel);
        let out = SHARED.flight_log[..(n as usize).min(FLIGHT_CAP)]
            .iter()
            .map(|s| s.load(Ordering::Acquire))
            .filter(|&v| v != 0)
            .map(|v| {
                (
                    (v >> 36) as u32,
                    base + (v & ((1 << 35) - 1)) - 1,
                    v >> 35 & 1 != 0,
                )
            })
            .collect();
        (out, n)
    }

    /// The host addresses of the first writes of the CPU to member pages after the flush of `frame` and before its commit,
    /// as logged (at most `P1_CAP`; the second number is how many there were). The log of the frame is emptied.
    pub fn take_between(&self, frame: u64) -> (Vec<u64>, u32) {
        let base = SHARED.base.load(Ordering::Acquire);
        let r = frame as usize % P1_RING;
        let n = SHARED.p1_n[r].swap(0, Ordering::AcqRel);
        let out = SHARED.p1_log[r][..(n as usize).min(P1_CAP)]
            .iter()
            .map(|s| s.load(Ordering::Acquire))
            .filter(|&v| v != 0)
            .map(|v| v - 1 + base)
            .collect();
        (out, n)
    }

    /// With a mirror, copies every page of a frame back into guest memory when the frame completes (it makes guest memory
    /// fresh, for bisecting a defect with the knobs that bind guest memory instead of the mirror).
    pub fn set_eager_sync(&self, on: bool) {
        SHARED.eager.store(on, Ordering::Release);
    }

    /// The newest frame whose completion was reported.
    pub fn completed(&self) -> u64 {
        SHARED.completed.load(Ordering::Acquire) & FRAME_MASK
    }

    /// The member bitmap (bit `i` of word `i / 64` is set while page `i` is tracked) and its generation. The copy is
    /// remade only when a page joined or left since the last call.
    pub fn member_snapshot(&self) -> (std::sync::Arc<Vec<u64>>, u64) {
        let gen_ = SHARED.member_gen.load(Ordering::Acquire);
        let mut c = ctrl();
        if let Some((g, a)) = &c.snap
            && *g == gen_
        {
            return (a.clone(), gen_);
        }
        let n = SHARED.npages.load(Ordering::Acquire).div_ceil(64);
        let p = SHARED.member_bits.load(Ordering::Acquire);
        let v: Vec<u64> = if p.is_null() {
            Vec::new()
        } else {
            // SAFETY: the bitmap has `n` words that are never freed.
            (0..n)
                .map(|i| unsafe { &*p.add(i) }.load(Ordering::Acquire))
                .collect()
        };
        let a = std::sync::Arc::new(v);
        c.snap = Some((gen_, a.clone()));
        (a, gen_)
    }

    /// True when every page of `[addr, addr + len)` is tracked (a member of the private set), so the mirror holds the
    /// bytes the GPU reads there. An empty or untracked range gives false.
    pub fn all_members(&self, addr: u64, len: u64) -> bool {
        let Some((first, end)) = page_span(addr, len) else {
            return false;
        };
        first < end
            && (first..end).all(|p| {
                word(p as usize).is_some_and(|w| w.load(Ordering::Acquire) & STATE_MASK != FREE)
            })
    }

    /// True when a page of the host range `[addr, addr + len)` is tagged with a frame: its guest bytes may be stale.
    pub fn any_gpu_owned(&self, addr: u64, len: u64) -> bool {
        let Some((first, end)) = page_span(addr, len) else {
            return false;
        };
        (first..end).any(|p| {
            word(p as usize).is_some_and(|w| {
                matches!(w.load(Ordering::Acquire) & STATE_MASK, GPU | OPEN | SYNC)
            })
        })
    }
}
