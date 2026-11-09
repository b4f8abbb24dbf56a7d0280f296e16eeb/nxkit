//! `NXKIT_DIAG=nativeprof`: the GPU time of each step of a native plan.
//!
//! The pass writes a precise timestamp before the first step and after every step into one heap of a small ring.
//! A heap is read when its turn comes round again, `RING` frames later, when the GPU has surely finished it. Every
//! `REPORT` frames the mean time of each kernel is printed, with the sum. Precise timestamps make the steps run one
//! after the other, so the sum is a little above the plan's time without them.

use std::sync::{Mutex, OnceLock, PoisonError};

use nxkit_metal::{ComputeCommand, ComputePass, Context, TimestampHeap};

/// Heaps in the ring: more than the frames the GPU can be behind.
const RING: usize = 6;
/// Frames between two reports.
const REPORT: u32 = 120;
/// Timestamps a heap holds: one before the first step and one after each step.
const ENTRIES: usize = 64;

struct Slot {
    heap: TimestampHeap,
    /// The kernels whose steps wrote the heap, in order, or empty when it holds nothing yet.
    kernels: Vec<&'static str>,
}

#[derive(Default)]
struct State {
    slots: Vec<Slot>,
    next: usize,
    /// Per kernel name: the summed time in ns and the number of steps.
    sums: Vec<(&'static str, u64, u32)>,
    frames: u32,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// Whether the profile is on.
pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::diag("nativeprof"))
}

/// A frame's profile: the heap its steps write and the index of the next timestamp.
pub(super) struct Frame {
    heap: TimestampHeap,
    slot: usize,
    index: u32,
    kernels: Vec<&'static str>,
}

/// Starts the profile of one plan run: reads the heap that comes round, then writes the first timestamp.
pub(super) fn begin(ctx: &Context, cp: &mut ComputePass<'_>) -> Option<Frame> {
    let mut s = state().lock().unwrap_or_else(PoisonError::into_inner);
    if s.slots.len() < RING {
        let heap = TimestampHeap::new(ctx, ENTRIES).ok()?;
        s.slots.push(Slot {
            heap,
            kernels: Vec::new(),
        });
    }
    let slot = s.next % s.slots.len();
    s.next = s.next.wrapping_add(1);
    let done = std::mem::take(&mut s.slots[slot].kernels);
    if !done.is_empty() {
        let heap = s.slots[slot].heap.clone();
        if let Some(ts) = heap.read_ns(done.len() + 1) {
            for (k, w) in done.iter().zip(ts.windows(2)) {
                let d = w[1].saturating_sub(w[0]);
                match s.sums.iter_mut().find(|(n, _, _)| n == k) {
                    Some(e) => {
                        e.1 += d;
                        e.2 += 1;
                    }
                    None => s.sums.push((k, d, 1)),
                }
            }
            s.frames += 1;
        }
        if s.frames >= REPORT {
            report(&s.sums, s.frames);
            s.sums.clear();
            s.frames = 0;
        }
    }
    let heap = s.slots[slot].heap.clone();
    drop(s);
    cp.encode(&ComputeCommand::Timestamp {
        heap: heap.clone(),
        index: 0,
    })
    .ok()?;
    Some(Frame {
        heap,
        slot,
        index: 1,
        kernels: Vec::new(),
    })
}

/// The timestamp after a step of `kernel`.
pub(super) fn step(f: &mut Frame, cp: &mut ComputePass<'_>, kernel: &'static str) {
    if f.index as usize >= ENTRIES {
        return;
    }
    if cp
        .encode(&ComputeCommand::Timestamp {
            heap: f.heap.clone(),
            index: f.index,
        })
        .is_ok()
    {
        f.index += 1;
        f.kernels.push(kernel);
    }
}

/// Ends the frame's profile: the heap is read when its slot comes round.
pub(super) fn end(f: Frame) {
    let mut s = state().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(slot) = s.slots.get_mut(f.slot) {
        slot.kernels = f.kernels;
    }
}

fn report(sums: &[(&'static str, u64, u32)], frames: u32) {
    let frames = u64::from(frames.max(1));
    let total: u64 = sums.iter().map(|(_, d, _)| d).sum();
    let mut line = format!(
        "DIAG nativeprof {} frames: total {:.2} ms",
        frames,
        total as f64 / frames as f64 / 1e6
    );
    for (k, d, n) in sums {
        line.push_str(&format!(
            ", {k} {:.2} ms ({}x)",
            *d as f64 / frames as f64 / 1e6,
            u64::from(*n) / frames
        ));
    }
    eprintln!("{line}");
}
