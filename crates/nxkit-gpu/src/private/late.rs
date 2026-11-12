//! Late pages: the CPU writes of a frame with forced submits, captured until their signal.

use super::*;

/// A staging buffer and the `(page, slot)` of the pages in it.
type Captured = (Arc<DataBuffer>, Vec<(u32, u32)>, Vec<(u32, [u64; 4])>);

/// The byte ranges a flushed frame reads (with the kind of binding), may write, and may write by source, for the report.
pub(super) type DiagSets = (Vec<(u64, u64, u8)>, Vec<(u64, u64)>, Vec<(u64, u64, u8)>);

/// A frame whose flush ran submits before the CPU signals they wait for: the capture of the CPU's writes stays open until
/// the signals come (the data those submits read is written until then).
#[derive(Default)]
struct LateFrame {
    /// The signals the frame still waits for; negative when signals came before the flush counted them.
    waiting: i32,
    /// The flush counted the signals.
    expected: bool,
    /// The capture ran. `staged` holds the pages: the buffer and `(page, slot)`.
    done: bool,
    staged: Option<Captured>,
}

static LATE: Mutex<Vec<(u64, LateFrame)>> = Mutex::new(Vec::new());
static LATE_CV: std::sync::Condvar = std::sync::Condvar::new();
/// Consecutive frames whose signals did not come in time, and the number after which the capture stops waiting for them.
pub(super) static LATE_MISSED: AtomicU64 = AtomicU64::new(0);
const LATE_MISSED_MAX: u64 = 4;
/// How long the encode of a frame waits for the signals of its forced submits.
pub(super) const LATE_WAIT: std::time::Duration = std::time::Duration::from_millis(15);

/// True when the capture still waits for CPU signals (it stops after [`LATE_MISSED_MAX`] frames in a row whose signals did
/// not come in time: the game does not signal what the submits wait for, or very late).
pub(crate) fn late_enabled() -> bool {
    mirrored() && LATE_MISSED.load(Ordering::Relaxed) < LATE_MISSED_MAX
}

fn late_entry(v: &mut Vec<(u64, LateFrame)>, fence: u64) -> &mut LateFrame {
    if let Some(i) = v.iter().position(|(f, _)| *f == fence) {
        return &mut v[i].1;
    }
    v.push((fence, LateFrame::default()));
    if v.len() > 16 {
        v.remove(0);
    }
    let i = v.iter().position(|(f, _)| *f == fence).unwrap_or(0);
    &mut v[i].1
}

/// The pages the CPU wrote since the last capture go into a staging buffer for `fence`.
fn late_capture(fence: u64, fr: &mut LateFrame) {
    fr.done = true;
    let mut guard = locked(&state().mirror);
    let Some(ms) = guard.as_mut() else { return };
    let mut sink = Sink::new(&ms.ctx, &mut ms.pool);
    Pages::global().flush_late(&mut sink);
    let (buf, slots, skips, failed) = sink.finish();
    drop(sink);
    let Some(b) = buf else { return };
    if slots.is_empty() || failed {
        ms.pool.free.push(b);
        return;
    }
    ms.pool.busy.push((fence, b.clone()));
    drop(guard);
    locked(&state().window).late_staged += slots.len() as u64;
    fr.staged = Some((b, slots, skips));
}

/// The flush of `fence` counted `n` CPU signals that its forced submits wait for (before the flush captures anything, so a
/// signal that comes meanwhile is not lost).
pub(crate) fn late_expect(fence: u64, n: u32) {
    let mut v = locked(&LATE);
    let fr = late_entry(&mut v, fence);
    fr.expected = true;
    fr.waiting += n as i32;
    if fr.waiting <= 0 && !fr.done {
        late_capture(fence, fr);
        LATE_CV.notify_all();
    }
    locked(&state().window).late_frames += 1;
}

/// A signal that a submit of `fence` waited for came: the last one closes the capture.
pub(crate) fn late_signal(fence: u64) {
    let mut v = locked(&LATE);
    let fr = late_entry(&mut v, fence);
    fr.waiting -= 1;
    if fr.expected && fr.waiting <= 0 && !fr.done {
        late_capture(fence, fr);
        LATE_CV.notify_all();
    }
}

/// The jobs ([`CopyJob`]) of a second copy-in pass for the frame of `o`, to encode before its first
/// command buffer is committed:
///
/// - a frame whose flush ran submits before the CPU signals they wait for waits here (at most [`LATE_WAIT`]) until the
///   signals came, and gets the pages the CPU wrote until the last of them: those submits read what the CPU wrote for
///   them, which the flush could not capture;
/// - with the knob `privlatestage`, every frame gets the pages the CPU wrote since the flush, whatever they were written
///   for (for comparing; with the CPU ahead of the encoder it also hands the frame the data of the next one).
pub(crate) fn late_stage(o: &GuestOverlay) -> Vec<CopyJob> {
    if !mirrored() {
        return Vec::new();
    }
    let mut taken: Vec<Captured> = Vec::new();
    if o.forced() {
        let t0 = Instant::now();
        let fence = o.frame();
        let mut v = locked(&LATE);
        let mut timed_out = false;
        loop {
            let fr = late_entry(&mut v, fence);
            if fr.done {
                break;
            }
            let left = LATE_WAIT.saturating_sub(t0.elapsed());
            if left.is_zero() {
                timed_out = true;
                late_capture(fence, fr);
                break;
            }
            v = LATE_CV
                .wait_timeout(v, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let fr = late_entry(&mut v, fence);
        if let Some(s) = fr.staged.take() {
            taken.push(s);
        }
        drop(v);
        if timed_out {
            LATE_MISSED.fetch_add(1, Ordering::Relaxed);
        } else {
            LATE_MISSED.store(0, Ordering::Relaxed);
        }
        let mut w = locked(&state().window);
        w.late_wait_ns += t0.elapsed().as_nanos() as u64;
        w.late_timeouts += u64::from(timed_out);
    }
    if crate::knob("privlatestage") {
        let mut guard = locked(&state().mirror);
        if let Some(ms) = guard.as_mut() {
            let fence = o.frame();
            let mut sink = Sink::new(&ms.ctx, &mut ms.pool);
            Pages::global().flush_late(&mut sink);
            let (buf, slots, skips, failed) = sink.finish();
            drop(sink);
            if let Some(b) = buf {
                if slots.is_empty() || failed {
                    ms.pool.free.push(b);
                } else {
                    ms.pool.busy.push((fence, b.clone()));
                    drop(guard);
                    locked(&state().window).late_staged += slots.len() as u64;
                    taken.push((b, slots, skips));
                }
            }
        }
    }
    taken
        .iter()
        .flat_map(|(b, slots, skips)| {
            let base = b.gpu_address();
            slots.iter().filter_map(move |&(page, slot)| {
                let skip = skips
                    .binary_search_by_key(&page, |s| s.0)
                    .map_or([0; 4], |i| skips[i].1);
                o.locate(o.va_base() + u64::from(page) * PAGE)
                    .map(|(_, real, _)| (real, base + u64::from(slot) * PAGE, skip))
            })
        })
        .collect()
}
