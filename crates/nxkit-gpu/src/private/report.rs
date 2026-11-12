//! The report of the counters, printed every few hundred frames.

use super::*;

fn kind_sum(s: &PageStats, k: FaultKind) -> u64 {
    s.faults[k as usize].iter().sum()
}

pub(super) fn print_report(frame: u64) {
    let st = state();
    let pages = Pages::global();
    let now = pages.stats();
    let mut w = locked(&st.window);
    let base = w.base;
    let n = w.flushes.max(1) as f64;
    let page = pages.page_size() as f64;
    let mib = |p: f64| p * page / (1024.0 * 1024.0);
    let d = |k: FaultKind| kind_sum(&now, k) - kind_sum(&base, k);
    let by_origin = |k: FaultKind| {
        let g = |o: FaultOrigin| {
            now.faults[k as usize][o as usize] - base.faults[k as usize][o as usize]
        };
        format!(
            "guest {:.1} host {:.1} internal {:.1}",
            g(FaultOrigin::Guest) as f64 / n,
            g(FaultOrigin::Host) as f64 / n,
            g(FaultOrigin::Internal) as f64 / n
        )
    };
    let peak = st
        .peak_members
        .fetch_max(now.members, std::sync::atomic::Ordering::Relaxed)
        .max(now.members);
    eprintln!(
        "DIAG private track frame {frame}: members {} pages ({:.1} MiB, peak {:.1} MiB, {} new in {} frames), \
         the frame may write {:.0} pages ({:.2} MiB) in {:.1} runs, {} ranges over the limits",
        now.members,
        mib(now.members as f64),
        mib(peak as f64),
        w.new_members,
        w.flushes,
        w.written_pages as f64 / n,
        mib(w.written_pages as f64 / n),
        w.written_runs as f64 / n,
        w.clipped,
    );
    eprintln!(
        "DIAG private faults per frame: cpu write to a clean page {:.1} ({}), write in flight {:.1} ({}), \
         read in flight {:.1} ({}), other (races{}) {:.1}; in the handler {:.3} ms per frame",
        d(FaultKind::CleanWrite) as f64 / n,
        by_origin(FaultKind::CleanWrite),
        d(FaultKind::WriteInFlight) as f64 / n,
        by_origin(FaultKind::WriteInFlight),
        d(FaultKind::ReadInFlight) as f64 / n,
        by_origin(FaultKind::ReadInFlight),
        if mirrored() {
            " and pages copied from the mirror"
        } else {
            ""
        },
        (d(FaultKind::Other) + d(FaultKind::Sync)) as f64 / n,
        (now.handler_ns - base.handler_ns) as f64 / n / 1e6,
    );
    eprintln!(
        "DIAG private per frame: pages dirtied {:.1} ({:.2} MiB; {:.1} of them by system calls), \
         system call accesses in flight read {:.1} write {:.1}; flush {:.3} ms (plan {:.3}), \
         mprotect {:.1} calls ({:.1} in the flush) {:.3} ms outside the handler, failed {}, \
         log overflows {}{}",
        w.dirtied as f64 / n,
        mib(w.dirtied as f64 / n),
        (now.hook_dirtied - base.hook_dirtied) as f64 / n,
        (now.host_access[0] - base.host_access[0]) as f64 / n,
        (now.host_access[1] - base.host_access[1]) as f64 / n,
        w.flush_ns as f64 / n / 1e6,
        w.plan_ns as f64 / n / 1e6,
        (now.mprotects - base.mprotects) as f64 / n,
        w.mprotects as f64 / n,
        (now.mprotect_ns - base.mprotect_ns) as f64 / n / 1e6,
        now.mprotect_failed - base.mprotect_failed,
        now.log_overflows - base.log_overflows,
        if now.pc_overflow > base.pc_overflow {
            format!(
                ", program counters over the table {}",
                now.pc_overflow - base.pc_overflow
            )
        } else {
            String::new()
        },
    );
    if mirrored() {
        let (usable, runs, retired) = locked(&st.mirror).as_ref().map_or((0, 0, 0), |m| {
            (
                m.mirror.usable_bytes(),
                m.mirror.runs().len(),
                m.mirror.retired(),
            )
        });
        let partial_now = crate::overlay::partial_table();
        let partial: u64 = partial_now.iter().flatten().sum::<u64>()
            - w.partial_base.iter().flatten().sum::<u64>();
        let sync = |o: FaultOrigin| {
            now.faults[FaultKind::Sync as usize][o as usize]
                - base.faults[FaultKind::Sync as usize][o as usize]
        };
        eprintln!(
            "DIAG private mirror frame {frame}: mirror {:.1} MiB usable in {runs} runs ({retired} buffers retired), \
             members {} pages; per frame: staged {:.1} pages ({:.2} MiB, {:.2} written again), copy-in {:.1} pages \
             (lost {}), pages copied mirror to guest {:.1} (by a fault of guest code {:.1}), closure added {:.1} pages, \
             waits for a writer {:.2} ({:.3} ms; {} timeouts), internal opens {:.2}, enter waits {}, \
             partial resolves {} in the window, poison {}",
            usable as f64 / (1024.0 * 1024.0),
            now.members,
            w.staged as f64 / n,
            mib(w.staged as f64 / n),
            w.raced as f64 / n,
            w.copy_in_pages as f64 / n,
            w.copy_in_lost,
            (now.synced - base.synced) as f64 / n,
            sync(FaultOrigin::Guest) as f64 / n,
            w.closure_pages as f64 / n,
            (now.writer_waits - base.writer_waits) as f64 / n,
            (now.writer_wait_ns - base.writer_wait_ns) as f64 / n / 1e6,
            now.wait_timeouts - base.wait_timeouts,
            (now.internal_opens - base.internal_opens) as f64 / n,
            w.enter_waits,
            partial,
            if poison_on() { "on" } else { "off" },
        );
    }
    if mirrored() {
        let t = crate::overlay::partial_table();
        let mut parts: Vec<String> = Vec::new();
        for (c, row) in t.iter().enumerate() {
            for (k, v) in row.iter().enumerate() {
                let d = v - w.partial_base[c][k];
                if d != 0 {
                    parts.push(format!(
                        "{} {}: {:.2}",
                        crate::overlay::CLASS_NAMES[c],
                        crate::overlay::SPLIT_NAMES[k],
                        d as f64 / n
                    ));
                }
            }
        }
        eprintln!(
            "DIAG private partial resolves per frame: {} ({} ranges learned for the closure, {} partial resolves of a \
             learned range in the window: a limit of the closure kept it split)",
            if parts.is_empty() {
                "none".to_string()
            } else {
                parts.join(", ")
            },
            crate::overlay::partial_range_count(),
            crate::overlay::partial_repeats() - w.repeats_base
        );
        let ms = |v: u64| v as f64 / n / 1e6;
        let handler = (now.handler_ns - base.handler_ns) as f64 / n / 1e6;
        let waited = (now.writer_wait_ns - base.writer_wait_ns) as f64 / n / 1e6;
        eprintln!(
            "DIAG private cost per frame, game thread: flush {:.3} ms = plan of written ranges {:.3} + read ranges {:.3} + \
             closure {:.3} + mirror growth {:.3} + pages {:.3} (new members {:.3}, dirty log {:.3}, staging the dirty pages \
             {:.3}, writable pages {:.3}, tags of the written pages {:.3}; mprotect in all of it {:.3}) + rest {:.3}; fault handler {:.3} ms (of it \
             waiting for writers {:.3}); the model costs {:.3} ms per frame",
            ms(w.t_total),
            ms(w.t_plan_written),
            ms(w.t_reads),
            ms(w.t_closure),
            ms(w.t_ensure),
            ms(w.t_pages),
            ms(w.t_new),
            ms(w.t_log),
            ms(w.t_stage),
            ms(w.t_hot),
            ms(w.t_tag),
            ms(w.t_prot),
            ms(w.t_rest),
            handler,
            waited,
            ms(w.t_total) + handler,
        );
        eprintln!(
            "DIAG private adaptive per frame: writable pages {:.1} (staged {:.1}, unchanged {:.1}; {:.2} promoted, {:.2} \
             protected again); copy-back after completion {:.1} pages in {:.2} mprotect calls, {:.3} ms on its own thread",
            w.hot_pages as f64 / n,
            w.hot_staged as f64 / n,
            w.hot_same as f64 / n,
            w.promoted as f64 / n,
            w.demoted as f64 / n,
            (now.eager_pages - base.eager_pages) as f64 / n,
            (now.eager_runs - base.eager_runs) as f64 / n,
            (now.eager_ns - base.eager_ns) as f64 / n / 1e6,
        );
        let ph = |i: usize| (now.phase_writes[i] - base.phase_writes[i]) as f64 / n;
        let longest = pages.take_wait_max();
        let waits = |k: usize| {
            format!(
                "{:.2} ({:.2} ms, longest {:.2} ms, {} began before the commit)",
                (now.waits[k] - base.waits[k]) as f64 / n,
                (now.wait_ns[k] - base.wait_ns[k]) as f64 / n / 1e6,
                longest[k] as f64 / 1e6,
                now.wait_uncommitted[k] - base.wait_uncommitted[k],
            )
        };
        let full = |i: usize| WAITS.full[i].swap(0, Ordering::Relaxed);
        let (early, bounded) = (
            WAITS.early.swap(0, Ordering::Relaxed),
            WAITS.bounded.swap(0, Ordering::Relaxed),
        );
        let (bounded_ns, bounded_max) = (
            WAITS.bounded_ns.swap(0, Ordering::Relaxed),
            WAITS.bounded_max_ns.swap(0, Ordering::Relaxed),
        );
        eprintln!(
            "DIAG private fences frame {frame}: {early} waits answered at the flush ({bounded} of them also waited for \
             the frame {} before (NXKIT_PRIVATE_AHEAD), {:.3} ms per frame, longest {:.2} ms); full waits: privhonest {}, copy-back {}, \
             idle GPU {}, flush not finished {}",
            run_ahead(),
            bounded_ns as f64 / n / 1e6,
            bounded_max as f64 / 1e6,
            full(0),
            full(1),
            full(2),
            full(3),
        );
        eprintln!(
            "DIAG private CPU waits for a writer, per frame: read in flight {}; write in flight {}",
            waits(0),
            waits(1),
        );
        if pages.late() {
            let g = |f: fn(&PageStats) -> u64| f(&now) - f(&base);
            let (reads, writes) = (g(|s| s.late_reads), g(|s| s.late_writes));
            let hist = |i: usize| now.late_age[i] - base.late_age[i];
            eprintln!(
                "DIAG private late results, per frame: late reads {:.2}, late writes {:.2} ({:.2} blocks taken from the frame, \
                 {:.2} blocks of copies left alone for them); age of the bytes served: mean {:.2} frames, longest {} (1 frame {}, \
                 2 {}, 3 {}, 4 or more {}); copied at completions {:.2} pages ({:.1} blocks; {:.2} pages from \
                 the GPU's copy of the frame's end), by the flush {:.2} pages; blocks of CPU writes the frame changed \
                 (its bytes win) {:.2}, of them kept aside for the next flush {:.2}, lost {:.2}; waits for a writer \
                 {:.2} per frame (should be about 0)",
                reads as f64 / n,
                writes as f64 / n,
                g(|s| s.late_owned) as f64 / n,
                g(|s| s.late_kept) as f64 / n,
                g(|s| s.late_age_sum) as f64 / (reads + writes).max(1) as f64,
                pages.take_late_age_max(),
                hist(0),
                hist(1),
                hist(2),
                hist(3),
                g(|s| s.late_copied) as f64 / n,
                g(|s| s.late_copied_blocks) as f64 / n,
                g(|s| s.late_exact) as f64 / n,
                g(|s| s.late_flush_copies) as f64 / n,
                g(|s| s.late_frame_won) as f64 / n,
                g(|s| s.late_carried) as f64 / n,
                g(|s| s.late_carry_lost) as f64 / n,
                g(|s| s.writer_waits) as f64 / n,
            );
        }
        if pages.late() {
            let list = |f: fn(&PageStats) -> [u64; SnapWhy::COUNT]| {
                let (a, b) = (f(&now), f(&base));
                let v: Vec<String> = SnapWhy::ALL
                    .iter()
                    .filter(|w| a[**w as usize] > b[**w as usize])
                    .map(|w| {
                        format!(
                            "{} {:.2}",
                            w.name(),
                            (a[*w as usize] - b[*w as usize]) as f64 / n
                        )
                    })
                    .collect();
                if v.is_empty() {
                    "none".to_string()
                } else {
                    v.join(", ")
                }
            };
            eprintln!(
                "DIAG private snapshots, per frame: dropped: {}; late accesses that found none and waited, by the last reason: \
                 {}; pages left readable by the flush {:.1}, readable pages that lost it at a completion {:.2}; system call reads \
                 that went on without a wait {:.2}, system call accesses that waited {:.2}",
                list(|s| s.snap_resets),
                list(|s| s.snap_misses),
                (now.read_open - base.read_open) as f64 / n,
                (now.read_revoked - base.read_revoked) as f64 / n,
                (now.settle_late - base.settle_late) as f64 / n,
                (now.settle_waits - base.settle_waits) as f64 / n,
            );
        }
        let by_end = |k: usize| {
            format!(
                "{:.2} ({:.2} ms)",
                (now.wait_ends[k] - base.wait_ends[k]) as f64 / n,
                (now.wait_end_ns[k] - base.wait_end_ns[k]) as f64 / n / 1e6,
            )
        };
        eprintln!(
            "DIAG private pass signals, per frame: waits ended by the signal of the page's last pass (the frame still ran) {}, \
             by the completion of the whole frame {}; accesses that found their page already released when they first faulted (the signal came before the reader) {:.2}; {} CPU-read GPU results \
             known now, the set changed {} times in the window; frames cut {:.2} ({:.2} cuts each, at {:.0}% of the passes on \
             average, {:.1} pages released per frame, {} of them committed while the rest was encoded); readers per frame {:.2}, \
             without a cut {:.2}; planning {:.3} ms",
            by_end(0),
            by_end(1),
            (now.early_syncs - base.early_syncs) as f64 / n,
            now.readers,
            now.reader_changes - base.reader_changes,
            w.early_frames as f64 / n,
            w.early_cuts as f64 / w.early_frames.max(1) as f64,
            100.0 * w.early_pos / w.early_cuts.max(1) as f64,
            w.early_pages as f64 / n,
            w.early_ahead,
            w.early_readers as f64 / n,
            w.early_unserved as f64 / n,
            w.early_plan_ns as f64 / n / 1e6,
        );
        if pages.emulating() {
            let per = |f: AccessForm| now.emulated[f as usize] - base.emulated[f as usize];
            let part = |loads: bool| {
                let list: Vec<String> = AccessForm::ALL
                    .iter()
                    .filter(|f| f.is_load() == loads && per(**f) > 0)
                    .map(|f| format!("{} {:.2}", f.name(), per(*f) as f64 / n))
                    .collect();
                let sum: u64 = AccessForm::ALL
                    .iter()
                    .filter(|f| f.is_load() == loads)
                    .map(|f| per(*f))
                    .sum();
                format!(
                    "{:.2} ({})",
                    sum as f64 / n,
                    if list.is_empty() {
                        "none".to_string()
                    } else {
                        list.join(", ")
                    }
                )
            };
            let missed: Vec<String> = FallbackReason::ALL
                .iter()
                .filter(|r| now.fallbacks[**r as usize] > base.fallbacks[**r as usize])
                .map(|r| {
                    format!(
                        "{} {:.2}",
                        r.name(),
                        (now.fallbacks[*r as usize] - base.fallbacks[*r as usize]) as f64 / n
                    )
                })
                .collect();
            eprintln!(
                "DIAG private emulated accesses per frame (outside the bytes a frame in flight writes, done in the handler \
                 without a wait): loads {}; stores {}; {:.3} ms in the handler for them; pages the flush staged for the \
                 stores {:.2}; not emulated, they waited for the writer: {}",
                part(true),
                part(false),
                (now.emulated_ns - base.emulated_ns) as f64 / n / 1e6,
                (now.emu_staged - base.emu_staged) as f64 / n,
                if missed.is_empty() {
                    "none".to_string()
                } else {
                    missed.join(", ")
                },
            );
            let tops = pages.take_top_fallback_pcs(TOP);
            if !tops.is_empty() {
                let line: Vec<String> = tops
                    .iter()
                    .map(|&(pc, c)| {
                        format!(
                            "{} {:.1} (insn {:#010x})",
                            nxkit_metal::name_pc(pc),
                            c as f64 / n,
                            pages.insn_at(pc)
                        )
                    })
                    .collect();
                eprintln!(
                    "DIAG private top program counters, not emulated (per frame): {}",
                    line.join(", ")
                );
            }
        }
        eprintln!(
            "DIAG private writes to member pages per frame (first write of a page): after the GPU was done or before the \
             next flush {:.1}, between the flush and the commit {:.1}, after the commit before the GPU was done {:.1}; \
             staged a second time (late captures for forced submits, privlatestage) {:.1}",
            ph(0),
            ph(1),
            ph(2),
            w.late_staged as f64 / n,
        );
        let n_between = w.between_frames.max(1) as f64;
        if w.late_frames > 0 || !late_enabled() {
            eprintln!(
                "DIAG private forced submits: {} frames had a flush that ran submits before the CPU signals they wait for; the \
                 encodes waited {:.2} ms per such frame for the signals, {} of the waits ended at the limit of {} ms{}",
                w.late_frames,
                w.late_wait_ns as f64 / w.late_frames.max(1) as f64 / 1e6,
                w.late_timeouts,
                LATE_WAIT.as_millis(),
                if late_enabled() {
                    ""
                } else {
                    "; the capture no longer waits for them (signals missed)"
                },
            );
        }
        report_in_flight(&mut w);
        let by_kind: Vec<String> = BIND_NAMES
            .iter()
            .zip(&w.between_reads)
            .filter(|(_, c)| **c > 0)
            .map(|(k, c)| format!("{k} {:.2}", *c as f64 / n_between))
            .collect();
        let reads_total: u64 = w.between_reads.iter().sum();
        let changed: Vec<String> = BIND_NAMES
            .iter()
            .zip(&w.changed_ranges)
            .filter(|(_, c)| **c > 0)
            .map(|(k, c)| format!("{k} {:.2}", *c as f64 / n_between))
            .collect();
        eprintln!(
            "DIAG private between flush and commit, exact: bound read ranges per frame whose bytes in a page the CPU wrote \
             since the flush differ at the commit from what the frame sees: {} ({} bytes in all; {} ranges could not be \
             compared)",
            if changed.is_empty() {
                "none".to_string()
            } else {
                changed.join(", ")
            },
            w.changed_bytes,
            w.uncompared,
        );
        eprintln!(
            "DIAG private between flush and commit, per frame, first writes of the CPU to member pages: {:.2} in all: {:.2} in \
             bytes the frame writes, {:.2} in bytes it reads through a binding ({}), {:.2} in a page of one of its bindings but \
             outside its bytes, {:.2} in no binding of the frame; {} of {} frames had a flush that ran submits before the CPU \
             signal they wait for ({:.2} writes into read bytes per such frame)",
            w.between_pages as f64 / n_between,
            w.between_written as f64 / n_between,
            reads_total as f64 / n_between,
            if by_kind.is_empty() {
                "none".to_string()
            } else {
                by_kind.join(", ")
            },
            w.between_same_page as f64 / n_between,
            w.between_other as f64 / n_between,
            w.forced_frames,
            w.between_frames,
            w.forced_reads as f64 / w.forced_frames.max(1) as f64,
        );
        let tops = pages.take_top_between_pcs(TOP);
        if !tops.is_empty() {
            let line: Vec<String> = tops
                .iter()
                .map(|&(pc, c)| format!("{} {:.1}", nxkit_metal::name_pc(pc), c as f64 / n))
                .collect();
            eprintln!(
                "DIAG private top program counters, write between flush and commit (per frame): {}",
                line.join(", ")
            );
        }
    }
    for (kind, label) in [
        (FaultKind::WriteInFlight, "write in flight"),
        (FaultKind::ReadInFlight, "read in flight"),
        (FaultKind::CleanWrite, "write to a clean page"),
    ] {
        let tops = pages.take_top_pcs(kind, TOP);
        if tops.is_empty() {
            continue;
        }
        let line: Vec<String> = tops
            .iter()
            .map(|&(pc, c)| format!("{} {:.1}", nxkit_metal::name_pc(pc), c as f64 / n))
            .collect();
        eprintln!(
            "DIAG private top program counters, {label} (per frame): {}",
            line.join(", ")
        );
    }
    *w = Window {
        base: now,
        partial_base: crate::overlay::partial_table(),
        repeats_base: crate::overlay::partial_repeats(),
        ..Window::default()
    };
}
