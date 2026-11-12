//! Step 6: the cuts of a frame after the passes that write the pages game threads waited for.

use super::*;

/// How many extra command buffers a frame is cut into at most, for pass signals.
pub(crate) const EARLY_CUTS: usize = 3;

/// A cut of a frame: after pass `after_pass` the command buffers so far go to the GPU on their own, and when they are done the
/// writer of `pages` (runs of page numbers) is done too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EarlyCut {
    pub(crate) after_pass: usize,
    pub(crate) pages: Vec<(u32, u32)>,
}

/// What [`early_cuts`] found for a frame.
#[derive(Default)]
pub(crate) struct EarlyPlan {
    pub(crate) cuts: Vec<EarlyCut>,
    /// The CPU-read GPU results the planner knew, and how many of them no cut serves (a pass writes them through a pool that
    /// shaders wrote through raw pointers, or no pass of the frame writes them).
    pub(crate) readers: usize,
    pub(crate) blocked: usize,
}

/// True when frames are cut for pass signals (`NXKIT_PRIVATE=1`; the knob `privnosplit` turns it off).
pub(crate) fn early_enabled() -> bool {
    mirrored() && !crate::knob("privnosplit")
}

/// Page numbers to runs.
pub(super) fn runs_of_pages(pages: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &p in pages {
        match out.last_mut() {
            Some(last) if last.1 == p => last.1 = p + 1,
            _ => out.push((p, p + 1)),
        }
    }
    out
}

/// The cuts for `readers` (sorted page numbers): `pass_runs[i]` are the page runs that pass `i` may write, `blocked` the runs
/// that some pass writes without saying which (pools that shaders wrote through raw pointers) or that no pass was found for.
/// A reader goes to the cut after the last pass that writes its page. At most `max_cuts` cuts come out: the earliest
/// `max_cuts - 1` and the last one, the pages of the others join the next cut after them. A cut after the last pass is no
/// cut. Returns the cuts and how many readers no cut serves.
pub(crate) fn plan_cuts(
    readers: &[u32],
    pass_runs: &[Vec<(u32, u32)>],
    blocked: &[(u32, u32)],
    max_cuts: usize,
) -> (Vec<EarlyCut>, usize) {
    let mut last: Vec<Option<usize>> = vec![None; readers.len()];
    let mut stop = vec![false; readers.len()];
    let span = |runs: &[(u32, u32)], f: &mut dyn FnMut(usize)| {
        for &(a, b) in runs {
            let lo = readers.partition_point(|&p| p < a);
            let hi = readers.partition_point(|&p| p < b);
            (lo..hi).for_each(&mut *f);
        }
    };
    for (i, runs) in pass_runs.iter().enumerate() {
        span(runs, &mut |k| last[k] = Some(i));
    }
    span(blocked, &mut |k| stop[k] = true);
    let mut by_pass: std::collections::BTreeMap<usize, Vec<u32>> =
        std::collections::BTreeMap::new();
    let mut unserved = 0;
    for (k, &page) in readers.iter().enumerate() {
        match last[k] {
            Some(i) if !stop[k] && i + 1 < pass_runs.len() => {
                by_pass.entry(i).or_default().push(page)
            }
            _ => unserved += 1,
        }
    }
    let keys: Vec<usize> = by_pass.keys().copied().collect();
    let keep = max_cuts.max(1);
    let mut cuts: Vec<EarlyCut> = Vec::new();
    let mut pending: Vec<u32> = Vec::new();
    for (n, key) in keys.iter().enumerate() {
        pending.extend(by_pass.remove(key).unwrap_or_default());
        if n + 1 < keep || n + 1 == keys.len() {
            pending.sort_unstable();
            cuts.push(EarlyCut {
                after_pass: *key,
                pages: runs_of_pages(&pending),
            });
            pending.clear();
        }
    }
    (cuts, unserved)
}

/// The cuts of the frame of `fence`: finds the CPU-read GPU results that game threads waited for lately
/// ([`Pages::readers`]), the last pass of the frame that may write each, and the cuts after those passes.
pub(crate) fn early_cuts(
    frame: &nxkit_nvn::frame::Frame,
    plan: &nxkit_nvn::passes::PassPlan,
    detail: &FrameDetail,
    fence: u64,
    map: &AddressMap,
    learned: &[(u64, u64)],
) -> EarlyPlan {
    use nxkit_nvn::passes::PassKind;
    let pages = Pages::global();
    let readers = pages.readers(fence);
    if readers.is_empty() || plan.passes.len() < 2 {
        return EarlyPlan::default();
    }
    let t0 = Instant::now();
    let ords = crate::draws::ordinals_with_end(frame);
    let runs_of_ranges = |ranges: &[(u64, u64)]| -> Vec<(u32, u32)> {
        ranges
            .iter()
            .filter(|&&(_, len)| len <= RANGE_MAX_BYTES)
            .filter_map(|&(a, len)| {
                let len = len.max(1);
                let h = map.host_address(a, len).ok()?;
                pages.page_range(h, len)
            })
            .collect()
    };
    let rw = locked(&state().rw);
    let mut seen = [(0u64, 0u32, 0u32, 0u64); 64];
    let mut attributed: Vec<(u32, u32)> = Vec::new();
    let mut pass_runs: Vec<Vec<(u32, u32)>> = Vec::with_capacity(plan.passes.len());
    for pass in &plan.passes {
        if !matches!(
            pass.kind,
            PassKind::Render | PassKind::Compute | PassKind::Copy
        ) {
            pass_runs.push(Vec::new());
            continue;
        }
        let (Some(lo), Some(hi)) = (
            ords.get(pass.cmds.start as usize),
            ords.get(pass.cmds.end as usize),
        ) else {
            // A pass that names commands outside the frame: nothing can be said about it.
            return EarlyPlan {
                readers: readers.len(),
                blocked: readers.len(),
                ..Default::default()
            };
        };
        seen.fill((0, 0, 0, 0));
        let ranges = written_in(
            detail,
            &rw,
            lo.draw as usize..hi.draw as usize,
            lo.dispatch as usize..hi.dispatch as usize,
            lo.copy as usize..hi.copy as usize,
            &mut seen,
        );
        let runs = runs_of_ranges(&ranges);
        attributed.extend_from_slice(&runs);
        pass_runs.push(runs);
    }
    // What the flush counts as written, whatever the passes say: the pools that shaders wrote through raw pointers can be
    // written by any pass, and a range that no pass was found for belongs to no cut.
    let mut seen = vec![(0u64, 0u32, 0u32, 0u64); 4096];
    let whole = written_in(
        detail,
        &rw,
        0..detail.draws.len(),
        0..detail.dispatches.len(),
        0..detail.copies.len(),
        &mut seen,
    );
    drop(rw);
    let mut blocked = runs_of_ranges(
        &learned
            .iter()
            .map(|&(lo, hi)| (lo, hi.saturating_sub(lo)))
            .collect::<Vec<_>>(),
    );
    attributed.sort_unstable();
    let known = |p: u32| {
        let i = attributed.partition_point(|&(_, e)| e <= p);
        attributed.get(i).is_some_and(|&(s, _)| s <= p)
    };
    for (a, b) in runs_of_ranges(&whole) {
        let mut p = a;
        while p < b {
            if !known(p) {
                let from = p;
                while p < b && !known(p) {
                    p += 1;
                }
                blocked.push((from, p));
            } else {
                p += 1;
            }
        }
    }
    blocked.sort_unstable();
    let (cuts, unserved) = plan_cuts(&readers, &pass_runs, &blocked, EARLY_CUTS);
    let mut w = locked(&state().window);
    w.early_plan_ns += t0.elapsed().as_nanos() as u64;
    EarlyPlan {
        cuts,
        readers: readers.len(),
        blocked: unserved,
    }
}

/// Removes the pages of the GPU address ranges `excluded` from the cuts (they are written in the last command buffer, by the
/// copy-back of textures), and drops the cuts that serve no page any more.
pub(crate) fn early_exclude(cuts: &mut Vec<EarlyCut>, excluded: &[(u64, u64)], map: &AddressMap) {
    if excluded.is_empty() {
        return;
    }
    let pages = Pages::global();
    let mut ex: Vec<(u32, u32)> = excluded
        .iter()
        .filter_map(|&(s, e)| {
            let h = map.host_address(s, e.saturating_sub(s).max(1)).ok()?;
            pages.page_range(h, e.saturating_sub(s).max(1))
        })
        .collect();
    ex.sort_unstable();
    for cut in cuts.iter_mut() {
        let mut out: Vec<(u32, u32)> = Vec::new();
        for &(a, b) in &cut.pages {
            let mut from = a;
            for &(s, e) in ex.iter().filter(|&&(s, e)| s < b && a < e) {
                if s > from {
                    out.push((from, s));
                }
                from = from.max(e);
            }
            if from < b {
                out.push((from, b));
            }
        }
        cut.pages = out;
    }
    cuts.retain(|c| !c.pages.is_empty());
}

/// What the encode did with the plan of a frame, for the report.
pub(crate) fn note_split(plan: &EarlyPlan, passes: usize, ahead: bool) {
    let cuts = &plan.cuts;
    let mut w = locked(&state().window);
    w.early_readers += plan.readers as u64;
    w.early_unserved += plan.blocked as u64;
    if cuts.is_empty() {
        return;
    }
    w.early_frames += 1;
    w.early_cuts += cuts.len() as u64;
    w.early_ahead += u64::from(ahead);
    for c in cuts {
        w.early_pos += (c.after_pass + 1) as f64 / passes.max(1) as f64;
        w.early_pages += c.pages.iter().map(|&(a, b)| u64::from(b - a)).sum::<u64>();
    }
}

/// The callback of a cut: the writer of its pages in the frame of `fence` is done.
pub(crate) fn early_release(fence: u64, pages: Vec<(u32, u32)>) -> impl FnOnce() + Send + 'static {
    move || Pages::global().release_early(fence, &pages)
}
