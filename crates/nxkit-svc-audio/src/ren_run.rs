//! Rendering of the SDK's CPU renderer, the render thread, and the catch-up of `RequestUpdate`. It follows `hle_audren.c`
//! of nxnative.
//!
//! The SDK renders in the guest: it generates the command list, the CPU processes it, and the host reads the DeviceSink
//! out of the list. The render thread calls the SDK once per frame and plays the result on the mixer.

use crate::engine::{Buf, Notify, mixer};
use crate::host::{self, sym, sym_opt};
use crate::ren_cmd::{FRAME_MAX, capture};
use crate::ren_scan::{LOCK_MUTEX, SDK_LOCK, SDK_UNLOCK, UNLOCK_MUTEX, impl_sys_off, update_lock};
use crate::ren_state::{AR_MAX, CQ, Ren, Sym, T_AR, lk, of_sys, slot};
use crate::sys::call::{call, call_pair};
use crate::sys::mem;
use crate::sys::sem::Sem;
use crate::util::{RES_FAIL, args};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// The result of a manual render in the wrong mode.
const NOT_SUPPORTED: u32 = 0x40299;
const AR: &str = "_ZNK2nn5audio6server17AudioRenderSystem";
const EXEC_PUBLIC: &str = "_ZN2nn5audio29ExecuteAudioRendererRenderingENS0_19AudioRendererHandleE";
const GEN20: &str = "_ZN2nn5audio6server17AudioRenderSystem15GenerateCommandENS_4SpanISt4byteEE";
const GEN16: &str = "_ZN2nn5audio6server17AudioRenderSystem15GenerateCommandEPvm";
const UPDATE: &str = "_ZN2nn5audio6server17AudioRenderSystem6UpdateEPvmS3_mPKvm";

static EXEC: Sym = Sym::new(EXEC_PUBLIC);
static ORIG_UPDATE: Sym = Sym::new(UPDATE);
static WAKES: [OnceLock<Sem>; AR_MAX] = [const { OnceLock::new() }; AR_MAX];

fn rd_u32(addr: u64) -> u32 {
    mem::with_host(addr, 4, |b| {
        b.get(0..4)
            .map_or(0, |x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
    })
}

fn rd_u64(addr: u64) -> u64 {
    mem::with_host(addr, 8, |b| {
        b.get(0..8).map_or(0, |x| {
            u64::from_le_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]])
        })
    })
}

/// The SDK functions and layout that rendering needs. They are looked up once, at the first render, and never change.
struct Render {
    sdk20: bool,
    sink_id: i32,
    gen20: usize,
    mode: usize,
    device: usize,
    cbuf: usize,
    cbsz: usize,
    gen16: usize,
    mu_off: usize,
    lock: usize,
    unlock: usize,
    tmax: usize,
    setup: usize,
    process: usize,
}

impl Render {
    fn load() -> Render {
        let mut r = Render {
            sdk20: false,
            sink_id: 0,
            gen20: sym_opt(GEN20),
            mode: 0,
            device: 0,
            cbuf: 0,
            cbsz: 0,
            gen16: 0,
            mu_off: 0,
            lock: 0,
            unlock: 0,
            tmax: 0,
            setup: 0,
            process: 0,
        };
        r.init();
        r
    }

    fn init(&mut self) {
        if self.gen20 == 0 {
            self.mode = sym(&format!("{AR}16GetExecutionModeEv"));
            self.device = sym(&format!("{AR}18GetRenderingDeviceEv"));
            self.cbuf = sym(&format!("{AR}26GetCommandBufferWorkBufferEv"));
            self.cbsz = sym(&format!("{AR}30GetCommandBufferWorkBufferSizeEv"));
            self.gen16 = sym(GEN16);
        }
        let sdk20 = self.gen20 != 0;
        self.sdk20 = sdk20;
        self.mu_off = if sdk20 { 0x2d0 } else { 0x1f0 };
        self.sink_id = if sdk20 { 0x14 } else { 0x13 };
        let found = update_lock(self.mu_off);
        if let Some((off, _)) = found {
            self.mu_off = off;
        }
        let os_mutex = found.is_some_and(|f| f.1);
        if os_mutex {
            self.lock = sym(LOCK_MUTEX);
            self.unlock = sym(UNLOCK_MUTEX);
        } else {
            self.lock = sym(SDK_LOCK);
            self.unlock = sym(SDK_UNLOCK);
        }
        eprintln!(
            "nxkit-audio: audren: AudioRenderSystem in the SDK {} layout (update lock {} at +{:#x}), DeviceSink id {:#x}",
            if sdk20 { "20" } else { "16" },
            if os_mutex {
                "nn::os::MutexType"
            } else {
                "SdkMutexType"
            },
            self.mu_off,
            self.sink_id
        );
        self.tmax = sym("_ZN2nn5audio3dsp20CommandListProcessor17SetProcessTimeMaxEj");
        self.setup = sym("_ZN2nn5audio3dsp20CommandListProcessor5SetupEPKvm");
        self.process = sym("_ZN2nn5audio3dsp20CommandListProcessor7ProcessEv");
    }
}

/// The resolved SDK functions, shared by all renderers. Renderers do not wait for each other while they render: the
/// SDK's own update lock is the only exclusion, and the command layout is kept per renderer.
static RENDER: OnceLock<Render> = OnceLock::new();

/// Renders one frame of the system at `sys`: generates the commands, lets the CPU process them and reads the DeviceSink
/// into `out`, which holds the stereo samples afterwards. `out` is empty when `want` is false or the frame made no
/// sound. Returns the result code.
fn render_once(a: &Ren, sys: u64, want: bool, out: &mut Vec<i16>) -> u32 {
    let _one = lk(&a.render);
    let r = RENDER.get_or_init(Render::load);
    out.clear();
    let sdk20 = r.sdk20;
    let (mode, device) = if sdk20 {
        (rd_u32(sys + 0x4c), rd_u32(sys + 0x48))
    } else {
        (
            call(r.mode, args(&[sys])) as u32,
            call(r.device, args(&[sys])) as u32,
        )
    };
    // The original needs the CPU renderer in manual mode. The host made it so.
    if mode != 1 || device != 1 {
        return NOT_SUPPORTED;
    }
    let t0 = Instant::now();
    // Command generation excludes the game thread's RequestUpdate, which holds the same mutex. Processing stays outside.
    let lock_at = sys + r.mu_off as u64;
    call(r.lock, args(&[lock_at]));
    let (cl, n) = if sdk20 {
        call_pair(
            r.gen20,
            args(&[sys, rd_u64(sys + 0xf8), rd_u64(sys + 0x100)]),
        )
    } else {
        let cl = call(r.cbuf, args(&[sys]));
        let size = call(r.cbsz, args(&[sys]));
        (cl, call(r.gen16, args(&[sys, cl, size])))
    };
    call(r.unlock, args(&[lock_at]));
    a.renders.fetch_add(1, Ordering::AcqRel);
    // The CommandListProcessor: 0x58 bytes, 0x60 in SDK 21.
    let mut processor = [0u128; 8];
    let p = processor.as_mut_ptr() as u64;
    call(r.tmax, args(&[p, 0xffff_ffff]));
    call(r.setup, args(&[p, cl, n]));
    call(r.process, args(&[p]));
    std::hint::black_box(&mut processor);
    let mut st = lk(&a.lay);
    if want {
        if !st.sink_set {
            st.layout.set_sink_id(r.sink_id);
            st.sink_set = true;
        }
        out.resize(FRAME_MAX * 2, 0);
        let nout = mem::with_host(cl, n, |s| {
            let mut cap = lk(&a.cap);
            capture(&mut st.layout, &mut cap, a.idx, s, out)
        });
        out.truncate(nout * 2);
    }
    let d = t0.elapsed();
    st.sum += d;
    st.max = st.max.max(d);
    st.count += 1;
    if st.count == 2000 {
        eprintln!(
            "nxkit-audio: audren: renderer {} render time mean {:.2} ms, max {:.2} ms (5 ms budget per frame, last 2000 frames), catch-up renders so far {}",
            a.idx,
            st.sum.as_secs_f64() * 1000.0 / 2000.0,
            st.max.as_secs_f64() * 1000.0,
            a.catchups.load(Ordering::Relaxed)
        );
        st.sum = Duration::ZERO;
        st.max = Duration::ZERO;
        st.count = 0;
    }
    0
}

/// A render from the render thread names the thread's renderer and its system. A game that executes its own CPU
/// renderer renders into that renderer's frame, which nothing plays.
fn ar_exec(sys: u64) -> u32 {
    let own = T_AR.with(|t| t.borrow().clone());
    let a = match own {
        Some(a) => {
            let _ = a
                .sys
                .compare_exchange(0, sys, Ordering::AcqRel, Ordering::Acquire);
            Some(a)
        }
        None => of_sys(sys),
    };
    let Some(a) = a else {
        eprintln!("nxkit-audio: [RED] audren: a render of a renderer the host did not open");
        return RES_FAIL;
    };
    let mut frame = lk(&a.frame);
    render_once(&a, sys, true, &mut frame)
}

/// The take-over of `AudioRendererImpl::ExecuteAudioRendererRendering`.
pub(crate) extern "C" fn h_impl_exec(imp: u64) -> u32 {
    ar_exec(imp + impl_sys_off() as u64)
}

/// The take-over of `AudioRenderSystem::ExecuteAudioRendererRendering`, which SDK 20 reaches through the PLT.
pub(crate) extern "C" fn h_ars_exec(sys: u64) -> u32 {
    ar_exec(sys)
}

/// Copies `f` into `blk`, or silence when `f` is empty. Returns the number of stereo frames.
fn fill(blk: &mut [i16], f: &[i16]) -> usize {
    let want = if f.is_empty() { 240 } else { f.len() / 2 };
    let n = want.min(blk.len() / 2);
    for (i, d) in blk.iter_mut().take(n * 2).enumerate() {
        *d = f.get(i).copied().unwrap_or(0);
    }
    n
}

/// Where a render thread's frames go: four host blocks on a mixer output.
extern "C" fn render_main(idx: u64) {
    let Some(a) = slot(idx as usize) else { return };
    T_AR.with(|t| *t.borrow_mut() = Some(a.clone()));
    let wake = WAKES[a.idx].get_or_init(Sem::new);
    let m = mixer();
    let Some(out) = m.open(2, Notify::Wake(wake)) else {
        eprintln!(
            "nxkit-audio: [RED] audren: no free output for renderer {}: it makes no sound",
            a.idx
        );
        return;
    };
    let mut blocks: Vec<Box<[i16]>> = (0..4)
        .map(|_| vec![0i16; FRAME_MAX * 2].into_boxed_slice())
        .collect();
    let mut free: Vec<usize> = (0..blocks.len()).collect();
    m.start(out);
    eprintln!(
        "nxkit-audio: audren: render thread {} started ({} buffers in flight, paced by the output)",
        a.idx,
        blocks.len()
    );
    let exec = EXEC.addr();
    let mut clicks = Clicks::default();
    while a.is_open() {
        // Wait for the output to hand a block back. It signals each release.
        while free.is_empty() {
            if let Some(b) = m.released(out) {
                free.push(b.token as usize);
                break;
            }
            wake.wait_ms(10);
            if !a.is_open() {
                break;
            }
        }
        let Some(bi) = free.pop() else { break };
        let from_queue = a.pop_cq();
        let from_queue_flag = from_queue.is_some();
        let blk = &mut blocks[bi];
        let n = match from_queue {
            Some(f) => fill(blk, &f),
            None => {
                lk(&a.frame).clear();
                call(exec, args(&[a.h0, a.h1]));
                let f = lk(&a.frame);
                fill(blk, &f)
            }
        };
        clicks.check(&blk[..n * 2], from_queue_flag);
        let b = Buf {
            token: bi as u64,
            addr: blk.as_ptr() as u64,
            size: (n * 4) as u64,
            host: true,
        };
        if !m.append(out, b) {
            free.push(bi);
            wake.wait_ms(5);
        }
    }
    // Close gives every block back before it returns, so the blocks may be freed after it.
    m.close(out);
    drop(blocks);
    a.wake_updates();
}

/// Starts the render thread of `a` on a guest thread.
pub(crate) fn start_thread(a: &Arc<Ren>) {
    let t = host::spawn_thread(render_main, a.idx as u64, 9, 0x10000);
    a.tt.store(t, Ordering::Release);
}

/// Renders a catch-up frame on the thread of the update. Its output is queued for the render thread. The queue is
/// bounded, so catch-up frames keep real time: a full queue makes the update wait up to 50 ms for the render thread.
fn catch_up(a: &Ren, sys: u64) {
    let mut q = lk(&a.cq);
    if q.len() >= CQ {
        let deadline = Instant::now() + Duration::from_millis(50);
        while a.is_open() && q.len() >= CQ {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            q = match a.cq_cv.wait_timeout(q, left) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
    }
    let full = q.len() >= CQ;
    drop(q);
    if full {
        // Still full, so the output stopped. Render anyway and discard the output.
        static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() {
            eprintln!(
                "nxkit-audio: audren: renderer {}: catch-up queue full, frame discarded ({n} so far)",
                a.idx
            );
        }
        render_once(a, sys, false, &mut Vec::new());
    } else {
        // A catch-up frame happens only when the game updated twice without a render in between, so this allocation is
        // off the steady path.
        let mut frame = Vec::with_capacity(FRAME_MAX * 2);
        render_once(a, sys, true, &mut frame);
        lk(&a.cq).push_back(frame);
    }
}

/// The take-over of `AudioRenderSystem::Update`. When nothing was rendered since the last update, one frame is rendered
/// first on this thread, so that the effects get their initial commands. Then the SDK's update runs.
pub(crate) extern "C" fn h_update(
    sys: u64,
    o: u64,
    on: u64,
    p: u64,
    pn: u64,
    inp: u64,
    inn: u64,
) -> u32 {
    let orig = ORIG_UPDATE.addr();
    let go = || call(orig, args(&[sys, o, on, p, pn, inp, inn])) as u32;
    // Two renderers just opened and neither has rendered: at most one catch-up frame less.
    let Some(a) = of_sys(sys) else { return go() };
    let u = a.updates.fetch_add(1, Ordering::AcqRel) + 1;
    let r = a.renders.load(Ordering::Acquire);
    if a.updated.load(Ordering::Acquire) && r == a.last_r.load(Ordering::Acquire) {
        let c = a.catchups.fetch_add(1, Ordering::AcqRel) + 1;
        if c <= 3 {
            eprintln!(
                "nxkit-audio: audren: renderer {}: no frame rendered before update {u}, rendering a catch-up frame first ({c} so far)",
                a.idx
            );
        }
        catch_up(&a, sys);
    }
    let res = go();
    a.last_r
        .store(a.renders.load(Ordering::Acquire), Ordering::Release);
    a.updated.store(true, Ordering::Release);
    res
}

/// `NXKIT_AUDIO_CLICKS=1`: finds jumps at the borders of the rendered blocks that are much larger than any step inside
/// the blocks around them, which is what a click sounds like, and prints each with the wall clock in ms (to put next
/// to the late frames of `NXKIT_PACE_DIAG`), whether the block was a catch-up frame, and the time since the last block.
#[derive(Default)]
struct Clicks {
    last: Option<[i16; 2]>,
    at: Option<Instant>,
    found: u32,
}

impl Clicks {
    fn check(&mut self, blk: &[i16], catch_up: bool) {
        static ON: OnceLock<bool> = OnceLock::new();
        if !*ON.get_or_init(|| std::env::var("NXKIT_AUDIO_CLICKS").is_ok_and(|v| v == "1"))
            || blk.len() < 4
        {
            return;
        }
        let now = Instant::now();
        let inner = blk
            .chunks_exact(2)
            .zip(blk.chunks_exact(2).skip(1))
            .map(|(a, b)| {
                (i32::from(b[0]) - i32::from(a[0]))
                    .abs()
                    .max((i32::from(b[1]) - i32::from(a[1])).abs())
            })
            .max()
            .unwrap_or(0);
        if let Some(prev) = self.last {
            let jump = (i32::from(blk[0]) - i32::from(prev[0]))
                .abs()
                .max((i32::from(blk[1]) - i32::from(prev[1])).abs());
            if jump > 2000 && jump > 3 * inner.max(1) && self.found < 2000 {
                self.found += 1;
                let ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() % 1_000_000);
                let gap = self
                    .at
                    .map_or(0.0, |t| now.duration_since(t).as_secs_f64() * 1e3);
                eprintln!(
                    "nxkit-audio: click t {ms}: jump {jump} (largest step inside {inner}), catch-up {catch_up}, {gap:.1} ms since the last block"
                );
            }
        }
        self.last = Some([blk[blk.len() - 2], blk[blk.len() - 1]]);
        self.at = Some(now);
    }
}
