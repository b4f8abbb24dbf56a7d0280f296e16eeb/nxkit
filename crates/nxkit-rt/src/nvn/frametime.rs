//! `NXKIT_DIAG=frame`: where the time of a late frame went on the threads that call NVN.
//!
//! Every present closes a frame. The time of the frame is split into the time in `nvnSyncWait`, in
//! `nvnQueueSubmitCommands` and `nvnQueueFlush`, in `nvnQueuePresentTexture`, and the rest (guest code and the NVN
//! calls that are not timed). The longest stretch between two timed calls is the longest piece of guest work. A frame
//! that takes more than 1.2 vsyncs of 60 Hz prints one line.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// What a timed call is.
#[derive(Clone, Copy)]
pub(crate) enum Part {
    /// `nvnSyncWait`.
    Wait = 0,
    /// `nvnQueueSubmitCommands` and `nvnQueueFlush`.
    Submit = 1,
    /// `nvnQueuePresentTexture`.
    Present = 2,
}

static SUM: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static CALLS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static MAX: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
/// Pieces of the frame: the present's flush, the backend's present, the submit's decode, the submit's wait
/// for its walk after the decode, and the window acquire (not part of the timed calls).
static SUB: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
/// The names of [`SUB`].
const SUB_NAMES: [&str; 5] = ["flush", "backend", "decode", "walk-join", "acquire"];

/// Runs `body` and counts its time as the piece `i` of [`SUB`].
pub(crate) fn sub<T>(i: usize, body: impl FnOnce() -> T) -> T {
    if !on() {
        return body();
    }
    let start = now();
    let v = body();
    SUB[i].fetch_add(now() - start, Ordering::Relaxed);
    v
}

/// The end of the last timed call, and the frame's longest stretch between timed calls, in ns since [`base`].
static LAST_END: AtomicU64 = AtomicU64::new(0);
static GAP: AtomicU64 = AtomicU64::new(0);
static FRAME_START: AtomicU64 = AtomicU64::new(0);

fn base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

fn now() -> u64 {
    u64::try_from(base().elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Whether the category is on.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| nxkit_nvn::diag::on("frame"))
}

/// Runs `body` and counts its time as `part`. A present also closes the frame.
pub(crate) fn timed<T>(part: Part, body: impl FnOnce() -> T) -> T {
    if !on() {
        return body();
    }
    let start = now();
    let last = LAST_END.load(Ordering::Relaxed);
    if last != 0 {
        GAP.fetch_max(start.saturating_sub(last), Ordering::Relaxed);
    }
    let v = body();
    let end = now();
    let i = part as usize;
    SUM[i].fetch_add(end - start, Ordering::Relaxed);
    CALLS[i].fetch_add(1, Ordering::Relaxed);
    MAX[i].fetch_max(end - start, Ordering::Relaxed);
    LAST_END.store(end, Ordering::Relaxed);
    if matches!(part, Part::Present) {
        close(end);
    }
    v
}

fn close(end: u64) {
    let start = FRAME_START.swap(end, Ordering::Relaxed);
    let take = |a: &[AtomicU64; 3]| a.each_ref().map(|x| x.swap(0, Ordering::Relaxed));
    let (sum, calls, max) = (take(&SUM), take(&CALLS), take(&MAX));
    let gap = GAP.swap(0, Ordering::Relaxed);
    let subs = SUB.each_ref().map(|x| x.swap(0, Ordering::Relaxed));
    if start == 0 {
        return;
    }
    let frame = end - start;
    if frame < 20_000_000 {
        return;
    }
    let ms = |ns: u64| ns as f64 / 1e6;
    let rest = frame.saturating_sub(sum.iter().sum());
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() % 1_000_000);
    eprintln!(
        "DIAG frame t {t} {:.2} ms: wait {:.2} ({} calls, max {:.2}) submit {:.2} ({} calls, max {:.2}) present {:.2} \
         rest {:.2} (longest stretch {:.2}) [{}]",
        ms(frame),
        ms(sum[0]),
        calls[0],
        ms(max[0]),
        ms(sum[1]),
        calls[1],
        ms(max[1]),
        ms(sum[2]),
        ms(rest),
        ms(gap),
        SUB_NAMES.iter().zip(subs).map(|(n, v)| format!("{n} {:.2}", ms(v))).collect::<Vec<_>>().join(" "),
    );
}
