//! Tests of the GPU context: fences, waits, finish, command buffer numbers and the release flush.

use super::*;
use nxkit_nvn::backend::{BackendResult, PoolInfo};
use nxkit_nvn::frame::Frame;
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::resource::TextureRecord;
use nxkit_nvn::tic::Tsc;

#[derive(Default)]
struct Spy {
    waits: Mutex<Vec<(Fence, u64)>>,
    idle: AtomicU64,
    /// The flushes that reached the backend: reason, fence and commands of the frame.
    flushes: Mutex<Vec<(FlushReason, Fence, usize)>>,
}

impl Backend for Spy {
    fn gpu_base(&self) -> u64 {
        0x10_0000_0000
    }
    fn register_pool(&self, _: PoolInfo) -> BackendResult<()> {
        Ok(())
    }
    fn unregister_pool(&self, _: u64) {}
    fn flush_frame(
        &self,
        frame: &Frame,
        _: &PassPlan,
        reason: FlushReason,
        fence: Fence,
        _: FlushCx<'_>,
    ) -> BackendResult<()> {
        if let Ok(mut f) = self.flushes.lock() {
            f.push((reason, fence, frame.cmds.len()));
        }
        Ok(())
    }
    fn present(&self, _: Present) -> BackendResult<()> {
        Ok(())
    }
    fn wait_fence(&self, fence: Fence, timeout_ns: u64) -> WaitResult {
        if let Ok(mut w) = self.waits.lock() {
            w.push((fence, timeout_ns));
        }
        WaitResult::Done
    }
    fn wait_idle(&self) {
        self.idle.fetch_add(1, Ordering::Relaxed);
    }
    fn texture_created(&self, _: &TextureRecord) {}
    fn texture_finalized(&self, _: TexId) {}
    fn sampler_registered(&self, _: u32, _: &Tsc) {}
    fn sampler_released(&self, _: u32) {}
}

fn gpu_with(spy: Arc<Spy>) -> Gpu {
    let space = LinearSpace::new(0x1000_0000, 0x10_0000_0000, 0x10_0000).expect("space");
    Gpu::new(space, spy, Settings::default())
}

#[test]
fn an_empty_frame_has_the_last_fence_and_flushes_nothing() {
    let spy = Arc::new(Spy::default());
    let g = gpu_with(spy.clone());
    assert_eq!(g.fence_now(), Fence(0));
    assert_eq!(g.flush(FlushReason::CpuWait).expect("flush"), Fence(0));
    assert_eq!(g.flushed(), 0);
}

#[test]
fn a_wait_on_a_done_fence_goes_to_the_backend_with_its_timeout() {
    let spy = Arc::new(Spy::default());
    let g = gpu_with(spy.clone());
    assert_eq!(g.wait(Fence(0), 7).expect("wait"), WaitResult::Done);
    assert_eq!(spy.waits.lock().expect("lock").as_slice(), &[(Fence(0), 7)]);
}

#[test]
fn the_end_of_a_recording_takes_no_lock_and_the_next_lock_applies_it() {
    let g = gpu_with(Arc::new(Spy::default()));
    let held = g.lock();
    // Another thread ends a recording while the present holds the lock: it returns at once.
    std::thread::scope(|s| {
        s.spawn(|| g.recorded_deferred(&[0x1000..0x2000, 0x8000..0x9000]))
            .join()
            .expect("thread");
    });
    assert_eq!(g.pending_recorded.lock().expect("lock").len(), 2);
    drop(held);
    drop(g.lock());
    assert!(
        g.pending_recorded.lock().expect("lock").is_empty(),
        "the lock applied the spans"
    );
}

#[test]
fn finish_waits_for_idle() {
    let spy = Arc::new(Spy::default());
    let g = gpu_with(spy.clone());
    g.finish().expect("finish");
    assert_eq!(spy.idle.load(Ordering::Relaxed), 1);
}

#[test]
fn command_buffer_numbers_are_stable_and_reused() {
    let g = gpu_with(Arc::new(Spy::default()));
    let a = g.command_buffer_index(0x1000);
    let b = g.command_buffer_index(0x2000);
    assert_ne!(a, b);
    assert_eq!(g.command_buffer_index(0x1000), a);
    g.command_buffer_release(0x1000);
    assert_eq!(g.command_buffer_index(0x3000), a);
}

#[test]
fn the_default_settings_are_api_55_15() {
    let s = Settings::default();
    assert_eq!((s.api_major, s.api_minor), (55, 15));
    assert!(s.shader_key_skip.is_none());
    let t = (s.clock_ns)();
    assert!((s.clock_ns)() >= t);
}

use nxkit_nvn::frame::Cmd;

fn work(g: &Gpu, n: usize) {
    g.lock()
        .frame_mut()
        .cmds
        .extend((0..n).map(|_| Cmd::BufferCopy));
}

fn sent(spy: &Spy) -> Vec<(FlushReason, u64, usize)> {
    let f = spy.flushes.lock().expect("lock");
    f.iter().map(|(r, f, n)| (*r, f.0, *n)).collect()
}

#[test]
fn a_release_that_brought_a_lot_of_work_flushes_the_frame_and_the_fences_follow() {
    let spy = Arc::new(Spy::default());
    let mut g = gpu_with(spy.clone());
    g.release_flush = order::ReleaseFlush {
        on: true,
        min_work: 4,
    };
    // Two commands were in the frame before the release; five came with it.
    work(&g, 2);
    let from = g.lock().frame().cmds.len();
    work(&g, 5);
    g.flush_released(&mut g.lock(), from).expect("flush");
    assert_eq!(sent(&spy), vec![(FlushReason::Release, 1, 7)]);
    assert_eq!(g.flushed(), 1);
    assert!(g.lock().frame_is_empty());
    // The work of the game after the release, and the present, are the next fence.
    assert_eq!(g.fence_now(), Fence(1));
    work(&g, 1);
    assert_eq!(g.fence_now(), Fence(2));
    assert_eq!(
        g.flush(FlushReason::Present).expect("present flush"),
        Fence(2)
    );
    assert_eq!(
        sent(&spy),
        vec![(FlushReason::Release, 1, 7), (FlushReason::Present, 2, 1)]
    );
    let stats = g.lock().flushes();
    assert_eq!((stats.release, stats.present), (1, 1));
}

#[test]
fn a_release_with_little_work_or_with_the_setting_off_flushes_nothing() {
    let spy = Arc::new(Spy::default());
    let mut g = gpu_with(spy.clone());
    g.release_flush = order::ReleaseFlush {
        on: true,
        min_work: 4,
    };
    work(&g, 3);
    g.flush_released(&mut g.lock(), 0).expect("flush");
    // The work before the release does not count: only the 2 commands that came with it.
    work(&g, 2);
    g.flush_released(&mut g.lock(), 3).expect("flush");
    g.release_flush.on = false;
    work(&g, 10);
    g.flush_released(&mut g.lock(), 5).expect("flush");
    assert!(sent(&spy).is_empty());
    assert_eq!(g.fence_now(), Fence(1));
}

#[test]
fn a_release_flush_leaves_the_held_ops_held() {
    let spy = Arc::new(Spy::default());
    let mut g = gpu_with(spy.clone());
    g.release_flush = order::ReleaseFlush {
        on: true,
        min_work: 1,
    };
    work(&g, 3);
    g.gate().push(
        0xa,
        gate::Op::Submit {
            handles: vec![CommandHandle(1)],
            origin: false,
        },
    );
    g.flush_released(&mut g.lock(), 0).expect("flush");
    assert_eq!(sent(&spy), vec![(FlushReason::Release, 1, 3)]);
    assert_eq!(g.gate().held.len(), 1, "the op that waits stays held");
    // Held work is work of the next flush: its fence is the next number.
    assert_eq!(g.fence_now(), Fence(2));
}
