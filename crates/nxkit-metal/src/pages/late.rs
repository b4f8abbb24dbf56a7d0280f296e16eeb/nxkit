//! Results one frame late: host reads of late pages, the copy-back at a completion and the carried CPU blocks.

use super::*;

/// True when a system call may read `[addr, addr + len)` of the page `pg` (tagged `GPU` with `w`) without waiting for the
/// writer: results are late, a frame in flight writes the page, its guest bytes are a completed frame's (`snap`, kept
/// current by the completions) and the read touches none of the blocks that frame writes.
pub(super) fn late_host_read(pg: usize, w: &AtomicU32, addr: u64, len: u64) -> bool {
    if !SHARED.late.load(Ordering::Relaxed) || SHARED.alias.load(Ordering::Acquire) == 0 {
        return false;
    }
    let cur = w.load(Ordering::Acquire);
    if cur & STATE_MASK != GPU || !page_flying(pg, cur >> FRAME_SHIFT) {
        return false;
    }
    let Some(i) = info(pg) else { return false };
    if i.snap.load(Ordering::Acquire) == 0 {
        return false;
    }
    let (base, p) = (SHARED.base.load(Ordering::Relaxed), page());
    let page_lo = base + pg as u64 * p;
    let lo = addr.max(page_lo) - page_lo;
    let hi = addr.saturating_add(len.max(1)).min(page_lo + p) - page_lo;
    if lo >= hi {
        return false;
    }
    let stale = i.stale();
    !(lo / BLOCK..=(hi - 1) / BLOCK).any(|b| stale[(b / 64) as usize] >> (b % 64) & 1 != 0)
}

/// True while game threads read the page `i` as a CPU-read GPU result: they faulted on it for a copy, or read it late, in
/// the last epochs.
pub(super) fn late_active(i: &PageInfo) -> bool {
    let e = i.eager.load(Ordering::Relaxed);
    e != 0 && SHARED.epoch.load(Ordering::Relaxed) <= e
}

/// True when the completion of `frame` (28 bits), the frame that still tags the page, copied its bytes into guest
/// memory already (results one frame late): a later copy from the mirror is not needed, and would be wrong, because the
/// mirror may hold the CPU's bytes that the next frame copied in where the completion gave the reader the frame's
/// bytes.
pub(super) fn late_current(i: &PageInfo, frame: u32) -> bool {
    SHARED.late.load(Ordering::Relaxed) && i.snap.load(Ordering::Acquire) == frame.max(1)
}

/// Takes the access to the page `pg` away again (`PROT_NONE`) when it is tagged GPU with the word `cur`: the page was left
/// readable for late readers and its guest bytes stop being a completed frame's. The claim keeps a thread that opens the
/// page at the same time from meeting the change halfway.
fn revoke_read(pg: usize, w: &AtomicU32, cur: u32) {
    if w.compare_exchange(
        cur,
        pack(SYNC, u64::from(cur >> FRAME_SHIFT)),
        Ordering::AcqRel,
        Ordering::Acquire,
    )
    .is_ok()
    {
        protect_pages(pg as u32, pg as u32 + 1, PROT_NONE, false);
        w.store(cur, Ordering::Release);
        wake_all();
        SHARED.read_revoked.fetch_add(1, Ordering::Relaxed);
    }
}

/// Results one frame late, at the completion of `frame` (the caller holds the control mutex): the pages of the frame's
/// `runs` whose guest bytes are the last completed frame's get the blocks that `frame` wrote copied into guest memory
/// through the alias, so that guest memory is the bytes of `frame` for the readers of the frame that runs now (a newer
/// frame already tags the page) or of the next one (`frame` is the newest tag). A page nobody read lately loses its
/// place, and with it the read access that the flush gave it.
///
/// The bytes come from the GPU's copy of the page at the end of the frame (`copies`, made after its last pass), not
/// from the mirror: by the time the completion runs, the next frame may have copied the CPU's staged bytes into the
/// mirror and written its own results there (a sum the CPU resets every frame would read back as the reset value). A
/// page without that copy uses the mirror only when no newer frame tags it, and otherwise loses its snapshot
/// (`SnapWhy::NoCopy`).
///
/// The CPU's own blocks (`PageInfo::keep`) stay, unless the frame changed them on the GPU (the copy at its start and at
/// its end differ): then the frame's bytes win in guest memory, as for any result read late. The CPU's bytes of such a
/// block reach the mirror all the same: a flush staged them already (`hold`), or they are kept aside in `carry` and the
/// next flush stages them (`own`).
pub(super) fn late_copy_back(
    frame: u64,
    runs: &[(u32, u32)],
    masks: Option<&[Vec<Mask>]>,
    copies: Option<&LateCopy>,
    carry: &mut BTreeMap<u32, Carry>,
    mirror: u64,
    alias: u64,
) {
    let p = page();
    let blocks = (p / BLOCK) as usize;
    let (mut pages, mut nblocks, mut exact, mut won_n, mut carried_n) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    for (ri, &(first, end)) in runs.iter().enumerate() {
        for pg in first..end {
            let Some(w) = word(pg as usize) else { continue };
            let cur = w.load(Ordering::Acquire);
            let state = cur & STATE_MASK;
            let Some(i) = info(pg as usize) else { continue };
            if matches!(state, SYNC | OPEN) {
                // A thread copies or reads the page right now: its bytes are no longer the last completed frame's.
                i.snap_reset(SnapWhy::Busy);
                continue;
            }
            let tag = u64::from(cur >> FRAME_SHIFT);
            if state != GPU || tag < frame & FRAME_MASK {
                continue;
            }
            // The newest tag is `frame` itself: no newer frame writes the page.
            let newest = tag == frame & FRAME_MASK;
            if i.snap.load(Ordering::Acquire) == 0 {
                continue;
            }
            let m = masks
                .and_then(|m| m.get(ri))
                .and_then(|r| r.get((pg - first) as usize));
            let Some(m) = m.filter(|_| late_active(i)) else {
                // The blocks of the frame are not known, or nobody reads the page: guest memory is not kept up to date.
                let why = if m.is_none() {
                    SnapWhy::Unknown
                } else {
                    SnapWhy::Idle
                };
                i.snap_reset(why);
                revoke_read(pg as usize, w, cur);
                continue;
            };
            let at = copies.and_then(|c| c.of(pg));
            if at.is_none() && !newest {
                i.snap_reset(SnapWhy::NoCopy);
                revoke_read(pg as usize, w, cur);
                continue;
            }
            let f = (frame & FRAME_MASK) as u32;
            // The page is claimed while it is copied: a thread that faults on it meanwhile (a store, or any access to a
            // page of a complete frame) waits for the claim instead of racing the copy.
            if w.compare_exchange(cur, pack(SYNC, tag), Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                i.snap_reset(SnapWhy::Busy);
                continue;
            }
            let keep = i.keep(f);
            let guest = alias + u64::from(pg) * p;
            let mut copy: Mask = std::array::from_fn(|k| m[k] & !keep[k]);
            let src = match at {
                Some((start, end_at)) => {
                    let mut won: Mask = [0; 4];
                    for b in (0..blocks).filter(|&b| has(m, b) && has(&keep, b)) {
                        let o = b as u64 * BLOCK;
                        // SAFETY: both copies hold a whole page for `pg`; the GPU finished them before the completion.
                        let (x, y) = unsafe {
                            (
                                std::slice::from_raw_parts(
                                    (start + o) as *const u8,
                                    BLOCK as usize,
                                ),
                                std::slice::from_raw_parts(
                                    (end_at + o) as *const u8,
                                    BLOCK as usize,
                                ),
                            )
                        };
                        if x != y {
                            won[b / 64] |= 1 << (b % 64);
                        }
                    }
                    let own = PageInfo::load_mask(&i.own);
                    let lift: Mask = std::array::from_fn(|k| won[k] & own[k]);
                    if lift.iter().any(|x| *x != 0) {
                        let c = carry.entry(pg).or_insert_with(|| Carry {
                            mask: [0; 4],
                            cpu: vec![0; p as usize],
                            frame: vec![0; p as usize],
                        });
                        copy_blocks(guest, c.cpu.as_mut_ptr() as u64, &lift);
                        copy_blocks(end_at, c.frame.as_mut_ptr() as u64, &lift);
                        for (k, l) in lift.iter().enumerate() {
                            c.mask[k] |= l;
                            i.own[k].fetch_and(!l, Ordering::AcqRel);
                        }
                        // The next flush stages the page, and the carried blocks go into the mirror with it.
                        if i.emu.fetch_or(1, Ordering::AcqRel) & 1 == 0 {
                            log_dirty(pg as usize);
                        }
                        carried_n += u64::from(lift.iter().map(|x| x.count_ones()).sum::<u32>());
                    }
                    for k in 0..4 {
                        copy[k] |= won[k];
                    }
                    won_n += u64::from(won.iter().map(|x| x.count_ones()).sum::<u32>());
                    exact += 1;
                    end_at
                }
                None => mirror + u64::from(pg) * p,
            };
            let kept: u32 = (0..4)
                .map(|k| (m[k] & keep[k] & !copy[k]).count_ones())
                .sum();
            copy_blocks(src, guest, &copy);
            i.snap.store(f.max(1), Ordering::Release);
            w.store(cur, Ordering::Release);
            wake_all();
            pages += 1;
            nblocks += u64::from(copy.iter().map(|c| c.count_ones()).sum::<u32>());
            if kept > 0 {
                SHARED
                    .late_kept
                    .fetch_add(u64::from(kept), Ordering::Relaxed);
            }
        }
    }
    SHARED.late_copied.fetch_add(pages, Ordering::Relaxed);
    SHARED
        .late_copied_blocks
        .fetch_add(nblocks, Ordering::Relaxed);
    SHARED.late_exact.fetch_add(exact, Ordering::Relaxed);
    SHARED.late_frame_won.fetch_add(won_n, Ordering::Relaxed);
    SHARED.late_carried.fetch_add(carried_n, Ordering::Relaxed);
}

/// Stages page `pg` (`bytes`, `skip`) with the CPU blocks a completion kept aside for it (`carry`, see
/// [`late_copy_back`]): a kept-aside block whose bytes here are still the frame's bytes that replaced the CPU's (no CPU
/// store since) is staged with the CPU's bytes and copied into the mirror.
pub(super) fn stage_carry(
    stager: &mut dyn Stager,
    carry: &mut BTreeMap<u32, Carry>,
    pg: u32,
    bytes: &[u8],
    skip: Mask,
) {
    let Some(c) = carry.remove(&pg) else {
        stager.stage_masked(pg, bytes, skip);
        return;
    };
    let mut skip = skip;
    let mut img: Option<Vec<u8>> = None;
    let n = (bytes.len() as u64 / BLOCK) as usize;
    for b in (0..n).filter(|&b| has(&c.mask, b)) {
        let r = b * BLOCK as usize..(b + 1) * BLOCK as usize;
        if bytes[r.clone()] == c.frame[r.clone()] {
            img.get_or_insert_with(|| bytes.to_vec())[r.clone()].copy_from_slice(&c.cpu[r]);
            skip[b / 64] &= !(1 << (b % 64));
        }
    }
    stager.stage_masked(pg, img.as_deref().unwrap_or(bytes), skip);
}
