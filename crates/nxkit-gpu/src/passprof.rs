//! `NXKIT_DIAG=passprof`: the GPU time of each encoded pass of a frame.
//!
//! Every pass writes a precise timestamp as the last command of its encoder, into entry `pass index` of one heap of
//! a small ring. A heap is read when its turn comes round again, `RING` frames later, when the GPU has surely
//! finished it. Precise timestamps make the passes run one after the other, so the time of a pass is its end
//! timestamp minus the end timestamp of the latest earlier pass that wrote one. The sum is a little above the
//! frame's time without the timestamps. Every `REPORT` frames the last read frame is printed with its most
//! expensive passes, with the mean total of the frames since the last report.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use nxkit_metal::{ComputeCommand, ComputePass, Context, RenderCommand, RenderPass, TimestampHeap};
use nxkit_nvn::passes::PassPlan;

/// Heaps in the ring: more than the frames the GPU can be behind.
const RING: usize = 6;
/// Frames between two reports.
const REPORT: u32 = 120;
/// Timestamps a heap holds: the passes with a higher index are not timed.
const ENTRIES: usize = 4096;
/// Passes in a report.
const TOP: usize = 40;

/// Whether the profile is on.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::diag("passprof"))
}

struct Slot {
    heap: TimestampHeap,
    /// The passes that wrote the heap: index and description. Empty when the heap holds nothing yet.
    passes: Vec<(u32, String)>,
}

#[derive(Default)]
struct State {
    slots: Vec<Slot>,
    next: usize,
    /// Frames read since the start.
    read: u64,
    /// Frames and summed total ns since the last report.
    frames: u32,
    total_ns: u64,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// A frame's profile. Cheap to clone and to share between the encoding threads.
#[derive(Clone)]
pub(crate) struct FrameProf {
    heap: TimestampHeap,
    slot: usize,
    /// The description of each pass, by index.
    descs: Arc<Vec<String>>,
    /// The passes whose end timestamp was encoded.
    written: Arc<Mutex<Vec<u32>>>,
    /// The first pass of each chunk (command buffer).
    starts: Arc<Mutex<Vec<u32>>>,
    /// The description of the entry after the last pass, the copy-back at the end of the frame.
    extra: Arc<Mutex<Option<String>>>,
}

impl FrameProf {
    /// The pass index of the copy-back at the end of the frame.
    pub(crate) fn extra_index(&self) -> usize {
        self.descs.len()
    }

    /// Describes the copy-back at the end of the frame.
    pub(crate) fn set_extra(&self, desc: String) {
        *self.extra.lock().unwrap_or_else(PoisonError::into_inner) = Some(desc);
    }

    /// Notes that the pass `index` is the first of a chunk: its time includes the gap before the buffer started.
    pub(crate) fn chunk_start(&self, index: usize) {
        self.starts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(index as u32);
    }

    fn mark(&self, index: usize) -> Option<u32> {
        // The index after the last pass is the copy-back at the end of the frame (`set_extra`).
        (index < ENTRIES && index <= self.descs.len()).then_some(index as u32)
    }

    fn note(&self, index: u32) {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(index);
    }

    /// The end timestamp of a render pass, the last command of its encoder.
    pub(crate) fn end_pass_render(&self, rp: &mut RenderPass<'_>, index: usize) {
        let Some(i) = self.mark(index) else { return };
        let cmd = RenderCommand::Timestamp {
            heap: self.heap.clone(),
            index: i,
        };
        if rp.encode(&cmd).is_ok() {
            self.note(i);
        }
    }

    /// The end timestamp of a compute or copy pass, the last command of its encoder.
    pub(crate) fn end_pass_compute(&self, cp: &mut ComputePass<'_>, index: usize) {
        let Some(i) = self.mark(index) else { return };
        let cmd = ComputeCommand::Timestamp {
            heap: self.heap.clone(),
            index: i,
        };
        if cp.encode(&cmd).is_ok() {
            self.note(i);
        }
    }
}

/// Starts the profile of a frame: reads the heap that comes round and takes it for this frame.
pub(crate) fn begin_frame(
    ctx: &Context,
    plan: &PassPlan,
    describe: impl Fn(usize) -> String,
) -> Option<FrameProf> {
    let mut s = state().lock().unwrap_or_else(PoisonError::into_inner);
    if s.slots.len() < RING {
        let heap = TimestampHeap::new(ctx, ENTRIES).ok()?;
        s.slots.push(Slot {
            heap,
            passes: Vec::new(),
        });
    }
    let slot = s.next % s.slots.len();
    s.next = s.next.wrapping_add(1);
    let done = std::mem::take(&mut s.slots[slot].passes);
    if !done.is_empty() {
        let heap = s.slots[slot].heap.clone();
        let max = done.iter().map(|(i, _)| *i as usize).max().unwrap_or(0);
        if let Some(ts) = heap.read_ns(max + 1) {
            account(&mut s, &done, &ts);
        }
    }
    let heap = s.slots[slot].heap.clone();
    drop(s);
    let descs: Vec<String> = (0..plan.passes.len().min(ENTRIES)).map(describe).collect();
    Some(FrameProf {
        heap,
        slot,
        descs: Arc::new(descs),
        written: Arc::new(Mutex::new(Vec::new())),
        starts: Arc::new(Mutex::new(Vec::new())),
        extra: Arc::new(Mutex::new(None)),
    })
}

/// Ends the frame's profile: the heap is read when its slot comes round.
pub(crate) fn end_frame(p: FrameProf) {
    let mut written =
        std::mem::take(&mut *p.written.lock().unwrap_or_else(PoisonError::into_inner));
    written.sort_unstable();
    written.dedup();
    let starts = std::mem::take(&mut *p.starts.lock().unwrap_or_else(PoisonError::into_inner));
    let passes = written
        .into_iter()
        .map(|i| {
            let mut d = match p.descs.get(i as usize) {
                Some(d) => d.clone(),
                None => p
                    .extra
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
                    .unwrap_or_default(),
            };
            if starts.contains(&i) {
                d.push_str(" [chunk start]");
            }
            (i, d)
        })
        .collect();
    let mut s = state().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(slot) = s.slots.get_mut(p.slot) {
        slot.passes = passes;
    }
}

/// The time of each pass from the end timestamps `(pass index, ns)`, in pass order. A pass whose value is 0 (not
/// written) or below the previous end (out of order) gets no time and is left out. The first timed pass has no
/// earlier end and gets 0.
fn durations(ends: &[(u32, u64)]) -> Vec<(u32, u64)> {
    let mut prev: Option<u64> = None;
    let mut out = Vec::with_capacity(ends.len());
    for &(i, t) in ends {
        if t == 0 || prev.is_some_and(|p| t < p) {
            continue;
        }
        out.push((i, prev.map_or(0, |p| t - p)));
        prev = Some(t);
    }
    out
}

fn account(s: &mut State, done: &[(u32, String)], ts: &[u64]) {
    let ends: Vec<(u32, u64)> = done
        .iter()
        .map(|(i, _)| (*i, ts.get(*i as usize).copied().unwrap_or(0)))
        .collect();
    let times = durations(&ends);
    let total: u64 = times.iter().map(|(_, d)| d).sum();
    s.read += 1;
    s.frames += 1;
    s.total_ns += total;
    if s.frames < REPORT {
        return;
    }
    let desc = |i: u32| {
        done.iter()
            .find(|(n, _)| *n == i)
            .map_or("", |(_, d)| d.as_str())
    };
    let mut kinds = [(0u64, 0u32); 3];
    for (i, d) in &times {
        let k = match desc(*i) {
            x if x.starts_with("render") => 0,
            x if x.starts_with("compute") => 1,
            x if x.starts_with("copy") => 2,
            _ => continue,
        };
        kinds[k].0 += d;
        kinds[k].1 += 1;
    }
    let ms = |ns: u64| ns as f64 / 1e6;
    eprintln!(
        "DIAG passprof frame {}: total {:.2} ms over {} passes; render {:.2} ms ({}), compute {:.2} ms ({}), copy {:.2} ms ({})",
        s.read,
        ms(total),
        times.len(),
        ms(kinds[0].0),
        kinds[0].1,
        ms(kinds[1].0),
        kinds[1].1,
        ms(kinds[2].0),
        kinds[2].1
    );
    let mut top = times;
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (i, d) in top.into_iter().take(TOP) {
        eprintln!("DIAG passprof   pass {i} {:.3} ms {}", ms(d), desc(i));
    }
    eprintln!(
        "DIAG passprof mean total {:.2} ms over {} frames",
        ms(s.total_ns / u64::from(s.frames)),
        s.frames
    );
    s.frames = 0;
    s.total_ns = 0;
}

#[cfg(test)]
mod tests {
    use super::durations;

    #[test]
    fn splits_the_frame_by_consecutive_ends() {
        let ends = [(0, 100), (1, 150), (2, 0), (3, 140), (4, 400)];
        // Pass 2 was not written and pass 3 ends before pass 1: neither gets a time.
        assert_eq!(durations(&ends), vec![(0, 0), (1, 50), (4, 250)]);
    }

    #[test]
    fn empty_input_gives_nothing() {
        assert!(durations(&[]).is_empty());
    }
}
