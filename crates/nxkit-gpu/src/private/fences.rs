//! The fences of encoded frames: the accesses between flush and completion and the frames in flight.

use super::*;

/// The encoder submitted the frame of `fence`: its pages are released when the batch is complete on the GPU. A frame
/// without a batch is complete at once.
pub(crate) fn encoded(fence: u64, batch: Option<&SubmittedBatch>) {
    if !on() {
        return;
    }
    // The frame is committed and runs; the completion says it is done.
    Pages::global().note_committed(fence);
    note_between(fence);
    let done = move || Pages::global().complete(fence);
    match batch {
        Some(b) => b.on_complete(done),
        None => done(),
    }
}

/// The commit of `fence` is reported: sorts the writes that the CPU first made to member pages since its flush by what the
/// frame binds. A write is in a byte range that the frame writes, in one it reads through a binding (by kind), in a page of
/// such a range but outside its bytes, or in no binding of the frame.
fn note_between(fence: u64) {
    if !crate::diag("private") {
        return;
    }
    drain_in_flight();
    let pages = Pages::global();
    let (writes, total) = pages.take_between(fence);
    let sets = locked(&state().sets)
        .iter()
        .find(|f| f.fence == fence)
        .cloned();
    let mut w = locked(&state().window);
    w.between_frames += 1;
    w.between_pages += u64::from(total);
    let Some(sets) = sets else {
        w.between_other += u64::from(total);
        return;
    };
    let psize = pages.page_size();
    let page_of = |a: u64| (a - pages.host_base()) / psize;
    let mut reads_forced = 0u64;
    for &a in &writes {
        if sets.written.iter().any(|&(s, e)| s <= a && a < e) {
            w.between_written += 1;
        } else if let Some(&(_, _, kind)) = sets.reads.iter().find(|&&(s, e, _)| s <= a && a < e) {
            w.between_reads[kind as usize] += 1;
            reads_forced += 1;
            if w.between_samples < 12 {
                w.between_samples += 1;
                let (s, e, _) = *sets
                    .reads
                    .iter()
                    .find(|&&(s, e, _)| s <= a && a < e)
                    .unwrap_or(&(0, 0, 0));
                eprintln!(
                    "DIAG private between sample: fence {fence} write at {a:#x} (offset {:#x} in its page) inside a {} binding \
                     {s:#x}..{e:#x} ({} bytes)",
                    (a - pages.host_base()) % psize,
                    BIND_NAMES[kind as usize],
                    e - s,
                );
            }
        } else if sets
            .written
            .iter()
            .any(|&(s, e)| page_of(s) <= page_of(a) && page_of(a) <= page_of(e - 1))
            || sets
                .reads
                .iter()
                .any(|&(s, e, _)| page_of(s) <= page_of(a) && page_of(a) <= page_of(e - 1))
        {
            w.between_same_page += 1;
        } else {
            w.between_other += 1;
        }
    }
    w.between_other += u64::from(total).saturating_sub(writes.len() as u64);
    // The exact test: the bytes of the bound ranges in those pages now against what the frame sees.
    let mut seen_pages: Vec<u64> = writes.iter().map(|&a| page_of(a)).collect();
    seen_pages.sort_unstable();
    seen_pages.dedup();
    for page in seen_pages {
        let pa = pages.host_base() + page * psize;
        for &(s, e, kind) in &sets.reads {
            let (lo, hi) = (s.max(pa), e.min(pa + psize));
            if lo >= hi {
                continue;
            }
            let mut now = vec![0u8; (hi - lo) as usize];
            let mut seen = vec![0u8; (hi - lo) as usize];
            if !pages.peek(lo, &mut now) {
                w.uncompared += 1;
                continue;
            }
            if !sets
                .view
                .read(sets.view.va_base() + (lo - pages.host_base()), &mut seen)
            {
                w.uncompared += 1;
                continue;
            }
            let diff = now.iter().zip(&seen).filter(|(a, b)| a != b).count() as u64;
            if diff > 0 {
                w.changed_ranges[kind as usize] += 1;
                w.changed_bytes += diff;
            }
        }
    }
    if sets.forced > 0 {
        w.forced_frames += 1;
        w.forced_reads += reads_forced;
    }
}

/// Sorts the first accesses of game threads to pages of frames in flight, as of now, by where the frame's right to write the
/// page comes from, into the window (`NXKIT_DIAG=private`). Called at each commit, while the sets of the frames are kept.
fn drain_in_flight() {
    let pages = Pages::global();
    let (log, _) = pages.take_in_flight();
    if log.is_empty() {
        return;
    }
    let psize = pages.page_size();
    let sets = locked(&state().sets).clone();
    let mut w = locked(&state().window);
    for &(frame, a, write) in &log {
        let pa = a - (a - pages.host_base()) % psize;
        let set = sets
            .iter()
            .find(|f| (f.fence & 0x0fff_ffff) as u32 == frame);
        let exact = set.and_then(|f| f.sources.iter().find(|&&(s, e, _)| s <= a && a < e));
        let (class, what) = match (set, exact) {
            (_, Some(&(s, e, 0))) => (
                0usize,
                format!("inside a binding or copy {s:#x}..{e:#x} ({} bytes)", e - s),
            ),
            (_, Some(&(s, e, _))) => (
                1,
                format!(
                    "inside a raw-store pool {s:#x}..{e:#x} ({} MiB)",
                    (e - s) >> 20
                ),
            ),
            (Some(f), None) => {
                let near = f
                    .sources
                    .iter()
                    .find(|&&(s, e, _)| s < pa + psize && pa < e);
                match near {
                    Some(&(s, e, _)) => (
                        2,
                        format!(
                            "outside the bytes of the range {s:#x}..{e:#x} that shares the page"
                        ),
                    ),
                    None => (2, "in no source of its frame".to_string()),
                }
            }
            (None, None) => (3, format!("frame {frame} not kept")),
        };
        w.flight_by_source[class][usize::from(write)] += 1;
        if let Some(e) = w.flight_pages.iter_mut().find(|e| e.0 == a) {
            e.1 += 1;
        } else if w.flight_pages.len() < 64 {
            w.flight_pages.push((a, 1, write, what));
        }
    }
}

/// `DIAG private in flight`: what game threads touched while the frame that tags it was in flight.
pub(super) fn report_in_flight(w: &mut Window) {
    let t = |c: usize| w.flight_by_source[c][0] + w.flight_by_source[c][1];
    if (0..4).all(|c| t(c) == 0) {
        return;
    }
    w.flight_pages
        .sort_unstable_by_key(|e| std::cmp::Reverse(e.1));
    let top: Vec<String> = w
        .flight_pages
        .iter()
        .take(6)
        .map(|(a, c, write, what)| {
            format!(
                "{a:#x} {c} first {} ({what})",
                if *write { "writes" } else { "reads" }
            )
        })
        .collect();
    eprintln!(
        "DIAG private in flight (first accesses of game threads to a page of a frame in flight, in the window; read/write): \
         at an address inside the bytes of a binding or copy the frame may write {}/{}, inside a raw-store pool {}/{}, outside \
         those bytes in a page they share (false sharing) {}/{}, the frame was not kept {}/{}; most touched addresses: {}",
        w.flight_by_source[0][0],
        w.flight_by_source[0][1],
        w.flight_by_source[1][0],
        w.flight_by_source[1][1],
        w.flight_by_source[2][0],
        w.flight_by_source[2][1],
        w.flight_by_source[3][0],
        w.flight_by_source[3][1],
        top.join("; "),
    );
}
