//! The counters of the tracker (`PageStats`, `FlushStats`) and the methods that read them.

use super::*;

/// The numbers of the tracker since the process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageStats {
    /// The pages that are tracked now.
    pub members: u64,
    /// Faults by `[kind][origin]`, with the kinds of [`FaultKind`] and the origins of [`FaultOrigin`].
    pub faults: [[u64; 3]; 5],
    /// Time that the handler ran for tracked pages.
    pub handler_ns: u64,
    /// Accesses of system calls to pages of a frame in flight (the hook): read, write.
    pub host_access: [u64; 2],
    /// Pages that went dirty through the hook.
    pub hook_dirtied: u64,
    /// Calls of `mprotect`, the time of those outside the handler, and the calls that failed.
    pub mprotects: u64,
    /// Time in `mprotect` outside the handler.
    pub mprotect_ns: u64,
    /// `mprotect` calls that failed.
    pub mprotect_failed: u64,
    /// Dirty pages that did not fit the log; the flush then scans the table.
    pub log_overflows: u64,
    /// Program counters that did not fit the tables.
    pub pc_overflow: u64,
    /// The signal of the last tracked fault: 10 `SIGBUS`, 11 `SIGSEGV`, 0 for none.
    pub last_signal: u64,
    /// Pages copied from the mirror into guest memory (mirrored mode).
    pub synced: u64,
    /// Waits for a writer that still ran, and the time they took.
    pub writer_waits: u64,
    /// Time of those waits.
    pub writer_wait_ns: u64,
    /// Accesses of internal threads to pages in flight, served without a wait.
    pub internal_opens: u64,
    /// Waits that ended at the timeout, with the page copied anyway.
    pub wait_timeouts: u64,
    /// Writes to member pages (first write of a page) by where the newest frame was: after its GPU work or before the
    /// next flush, between its flush and its commit, after its commit and before the GPU is done.
    pub phase_writes: [u64; 3],
    /// Waits of a thread for the writer of a page by access (0 read, 1 write), and their time.
    pub waits: [u64; 2],
    /// Time of those waits.
    pub wait_ns: [u64; 2],
    /// How many of those waits began before the frame was committed.
    pub wait_uncommitted: [u64; 2],
    /// Accesses the handler did itself, by [`Form`], and the time they took in the handler.
    pub emulated: [u64; Form::COUNT],
    /// Time of the emulated accesses.
    pub emulated_ns: u64,
    /// Accesses of a page in flight that the handler could not do, by [`Reason`]; they waited for the writer.
    pub fallbacks: [u64; Reason::COUNT],
    /// Pages the flush staged because the handler stored to them.
    pub emu_staged: u64,
    /// Pages the eager copy moved after a frame completed, `mprotect` calls it made and the time it took (on its own thread).
    pub eager_pages: u64,
    /// `mprotect` calls of the eager copy.
    pub eager_runs: u64,
    /// Time of the eager copy.
    pub eager_ns: u64,
    /// Waits for a writer by what ended them (0 the pass signal of the page while the frame still ran, 1 the completion of
    /// the whole frame or the timeout), and their time.
    pub wait_ends: [u64; 2],
    /// Time of those waits.
    pub wait_end_ns: [u64; 2],
    /// Calls of [`Pages::release_early`], the pages they released, and the accesses that found a page released while its frame
    /// still ran (they copied the page without a wait).
    pub early_calls: u64,
    /// Pages released by the pass signals.
    pub early_pages: u64,
    /// Accesses that found their page released.
    pub early_syncs: u64,
    /// The pages that are CPU-read GPU results now, and how often the set changed (a page joined or left).
    pub readers: u64,
    /// Changes of that set.
    pub reader_changes: u64,
    /// Results one frame late: accesses served from guest memory without a wait to blocks a frame in flight writes, loads
    /// and stores (a store of an atomic counts as a store).
    pub late_reads: u64,
    /// Late stores.
    pub late_writes: u64,
    /// The age of the bytes late accesses met, in frames, summed, and as a histogram of 1, 2, 3 and 4 or more frames.
    pub late_age_sum: u64,
    /// The histogram.
    pub late_age: [u64; 4],
    /// Blocks that late stores took from the frame in flight (the CPU's bytes win over its copy-back).
    pub late_owned: u64,
    /// Blocks a copy from the mirror left alone because the CPU wrote them.
    pub late_kept: u64,
    /// Pages and blocks the completion of a frame copied into guest memory for the next frame's readers, and the pages the
    /// flush copied itself.
    pub late_copied: u64,
    /// Blocks of those copies.
    pub late_copied_blocks: u64,
    /// Pages the flush copied.
    pub late_flush_copies: u64,
    /// Pages the completion copied from the GPU's copy of the frame's end (exact), not from the live mirror.
    pub late_exact: u64,
    /// Blocks the CPU wrote while the frame ran that the frame changed on the GPU: the frame's bytes replaced the CPU's
    /// in guest memory.
    pub late_frame_won: u64,
    /// Of those, blocks whose CPU bytes no flush had staged yet: they were kept aside for the next flush.
    pub late_carried: u64,
    /// Kept-aside blocks that no flush staged.
    pub late_carry_lost: u64,
    /// Snapshots dropped, by [`SnapWhy`].
    pub snap_resets: [u64; SnapWhy::COUNT],
    /// Late accesses that found no snapshot (they waited), by the reason the page lost it last.
    pub snap_misses: [u64; SnapWhy::COUNT],
    /// Pages the flush left readable while a frame in flight writes them, and the ones that lost it at a completion.
    pub read_open: u64,
    /// Pages that lost the read access at a completion.
    pub read_revoked: u64,
    /// Pages of system call reads (outside the stale blocks, of a page with a snapshot) that went on without a wait.
    pub settle_late: u64,
    /// Pages of system calls that waited for a writer.
    pub settle_waits: u64,
}

/// What one flush did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushStats {
    /// The pages that were dirty and went back to clean.
    pub dirtied: u32,
    /// The pages the new frame may write, and the runs they form.
    pub written: u32,
    /// Runs of the pages the new frame may write.
    pub written_runs: u32,
    /// Pages that became tracked in this flush.
    pub new_members: u32,
    /// `mprotect` calls and their time.
    pub mprotects: u32,
    /// Time in the flush.
    pub ns: u64,
    /// The log overflowed and the flush scanned the table.
    pub scanned: bool,
    /// Pages handed to the stager (mirrored mode).
    pub staged: u32,
    /// Pages that the CPU wrote between the staging and the tag (mirrored mode); they are staged again.
    pub raced: u32,
    /// Pages staged because the handler stored to them while a frame wrote others of their blocks (part of `staged`).
    pub emu_staged: u32,
    /// Step 5: pages that stay writable after this flush, the ones among them staged (part of `staged`) and the ones
    /// found unchanged, the pages that became writable and the ones protected again.
    pub hot: u32,
    /// Writable pages that differed from the bytes staged last and were staged.
    pub hot_staged: u32,
    /// Writable pages that were unchanged.
    pub hot_same: u32,
    /// Pages that became writable in this flush.
    pub promoted: u32,
    /// Pages that were protected again in this flush.
    pub demoted: u32,
    /// Where the flush spent its time (mirrored mode): new members, the log of dirty pages, staging with the protection of
    /// the dirty pages, the writable pages, and the tags of the pages the frame writes.
    pub new_ns: u64,
    /// Time collecting the dirty pages from the log.
    pub log_ns: u64,
    /// Time protecting and staging the dirty pages.
    pub stage_ns: u64,
    /// Time diffing the writable pages.
    pub hot_ns: u64,
    /// Time tagging the pages the frame writes.
    pub tag_ns: u64,
    /// Time in `mprotect` during the flush (part of the times above).
    pub mprotect_ns: u64,
}

impl Pages {
    /// The numbers since the process started.
    pub fn stats(&self) -> PageStats {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let mut faults = [[0u64; 3]; 5];
        for (k, row) in faults.iter_mut().enumerate() {
            for (o, v) in row.iter_mut().enumerate() {
                *v = g(&SHARED.faults[k][o]);
            }
        }
        let learned = {
            let l = LEARNED.lock().unwrap_or_else(PoisonError::into_inner);
            (l.pages.len() as u64, l.changes)
        };
        PageStats {
            members: g(&SHARED.members),
            faults,
            handler_ns: g(&SHARED.handler_ns),
            host_access: [g(&SHARED.host_access[0]), g(&SHARED.host_access[1])],
            hook_dirtied: g(&SHARED.hook_dirtied),
            mprotects: g(&SHARED.mprotects),
            mprotect_ns: g(&SHARED.mprotect_ns),
            mprotect_failed: g(&SHARED.mprotect_failed),
            log_overflows: g(&SHARED.log_overflows),
            pc_overflow: g(&SHARED.pc_overflow),
            last_signal: g(&SHARED.last_signal),
            synced: g(&SHARED.synced),
            writer_waits: g(&SHARED.writer_waits),
            writer_wait_ns: g(&SHARED.writer_wait_ns),
            internal_opens: g(&SHARED.internal_opens),
            wait_timeouts: g(&SHARED.wait_timeouts),
            phase_writes: [
                g(&SHARED.phase_writes[0]),
                g(&SHARED.phase_writes[1]),
                g(&SHARED.phase_writes[2]),
            ],
            waits: [g(&SHARED.wait_n[0]), g(&SHARED.wait_n[1])],
            wait_ns: [g(&SHARED.wait_ns[0]), g(&SHARED.wait_ns[1])],
            wait_uncommitted: [
                g(&SHARED.wait_uncommitted[0]),
                g(&SHARED.wait_uncommitted[1]),
            ],
            emulated: std::array::from_fn(|i| g(&SHARED.emu[i])),
            emulated_ns: g(&SHARED.emu_ns),
            fallbacks: std::array::from_fn(|i| g(&SHARED.fallback[i])),
            emu_staged: g(&SHARED.emu_staged),
            eager_pages: g(&SHARED.eager_pages),
            eager_runs: g(&SHARED.eager_runs),
            eager_ns: g(&SHARED.eager_ns),
            wait_ends: [g(&SHARED.wait_end[0]), g(&SHARED.wait_end[1])],
            wait_end_ns: [g(&SHARED.wait_end_ns[0]), g(&SHARED.wait_end_ns[1])],
            early_calls: g(&SHARED.early_calls),
            early_pages: g(&SHARED.early_pages),
            early_syncs: g(&SHARED.early_syncs),
            readers: learned.0,
            reader_changes: learned.1,
            late_reads: g(&SHARED.late_reads),
            late_writes: g(&SHARED.late_writes),
            late_age_sum: g(&SHARED.late_age_sum),
            late_age: std::array::from_fn(|i| g(&SHARED.late_age[i])),
            late_owned: g(&SHARED.late_owned),
            late_kept: g(&SHARED.late_kept),
            late_copied: g(&SHARED.late_copied),
            late_copied_blocks: g(&SHARED.late_copied_blocks),
            late_flush_copies: g(&SHARED.late_flush_copies),
            late_exact: g(&SHARED.late_exact),
            late_frame_won: g(&SHARED.late_frame_won),
            late_carried: g(&SHARED.late_carried),
            late_carry_lost: g(&SHARED.late_carry_lost),
            snap_resets: std::array::from_fn(|i| g(&SHARED.snap_resets[i])),
            snap_misses: std::array::from_fn(|i| g(&SHARED.snap_misses[i])),
            read_open: g(&SHARED.read_open),
            read_revoked: g(&SHARED.read_revoked),
            settle_late: g(&SHARED.settle_late),
            settle_waits: g(&SHARED.settle_waits),
        }
    }

    /// The instruction word at `pc`, a program counter of an access the tables counted, for the reports.
    pub fn insn_at(&self, pc: u64) -> u32 {
        if pc & 3 != 0 || pc == 0 {
            return 0;
        }
        // SAFETY: the program counter of a fault names an instruction that was fetched, so the word is readable (the
        // code of the game stays mapped while it runs).
        unsafe { (pc as *const u32).read() }
    }

    /// The longest wait for a writer since the last call, by access (0 read, 1 write), in ns.
    pub fn take_wait_max(&self) -> [u64; 2] {
        [
            SHARED.wait_max_ns[0].swap(0, Ordering::Relaxed),
            SHARED.wait_max_ns[1].swap(0, Ordering::Relaxed),
        ]
    }

    /// The `n` program counters that faulted most for `kind` (only the kinds with a table: a clean write, a read and a
    /// write in flight), most first, with their counts. The tables are emptied.
    pub fn take_top_pcs(&self, kind: FaultKind, n: usize) -> Vec<(u64, u64)> {
        let k = kind as usize;
        if k > 2 {
            return Vec::new();
        }
        self.take_top(k, n)
    }

    /// The `n` program counters that wrote member pages between a flush and the commit of its frame, most first. The table
    /// is emptied.
    pub fn take_top_between_pcs(&self, n: usize) -> Vec<(u64, u64)> {
        self.take_top(3, n)
    }

    /// The `n` program counters whose access to a page in flight was not emulated, most first. The table is emptied.
    pub fn take_top_fallback_pcs(&self, n: usize) -> Vec<(u64, u64)> {
        self.take_top(PC_FALLBACK, n)
    }

    fn take_top(&self, k: usize, n: usize) -> Vec<(u64, u64)> {
        let mut v: Vec<(u64, u64)> = Vec::new();
        for (key, c) in &SHARED.pcs[k] {
            let pc = key.load(Ordering::Relaxed);
            if pc != 0 {
                v.push((pc, c.swap(0, Ordering::Relaxed)));
                key.store(0, Ordering::Relaxed);
            }
        }
        v.sort_unstable_by_key(|&(pc, c)| (std::cmp::Reverse(c), pc));
        v.truncate(n);
        v
    }
}
