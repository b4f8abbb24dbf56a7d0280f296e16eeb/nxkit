//! Tests of the GPU context: fences, waits, finish, command buffer numbers, the release and the queue flush.

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
    /// The window of the virtual memory pools.
    window: Option<(u64, u64)>,
}

impl Backend for Spy {
    fn gpu_base(&self) -> u64 {
        0x10_0000_0000
    }
    fn virtual_window(&self) -> Option<(u64, u64)> {
        self.window
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

#[test]
fn a_queue_flush_sends_the_frame_leaves_the_held_ops_held_and_notes_the_submits() {
    let spy = Arc::new(Spy::default());
    let g = gpu_with(spy.clone());
    work(&g, 2);
    g.queue_flush.submitted();
    g.gate().push(
        0xa,
        gate::Op::Submit {
            handles: vec![CommandHandle(1)],
            origin: false,
        },
    );
    assert_eq!(g.flush(FlushReason::Queue).expect("flush"), Fence(1));
    assert_eq!(sent(&spy), vec![(FlushReason::Queue, 1, 2)]);
    assert_eq!(g.gate().held.len(), 1, "the op that waits stays held");
    assert_eq!(g.lock().flushes().queue, 1);
    assert_eq!(g.queue_flush.unsent(), 0, "the flush took the submit along");
}

fn residency(g: &Gpu, id: TexId) -> nxkit_nvn::resource::Residency {
    g.lock().resources().get(id).expect("record").residency
}

#[test]
fn a_sparse_pool_maps_pages_and_its_textures_load_again_when_they_change() {
    use nxkit_nvn::resource::Residency::{GuestNewer, InSync};
    let spy = Arc::new(Spy {
        window: Some((0x20_0000_0000, 0x100_0000)),
        ..Spy::default()
    });
    let g = gpu_with(spy);
    // the guest address of the NVNmemoryPool, a key only
    let pool = 0x5000;
    let base = g.vpool_init(pool, 0x2_0000).expect("a range");
    assert_eq!(base, 0x20_0000_0000);
    assert_eq!(g.vpool_take(pool), Some(base));
    // An address of the range left the host: a request that keeps the offsets of a physical pool makes it sparse.
    assert_eq!(g.vpool_decide(pool, true), Some(VMode::Sparse));
    let desc = nxkit_nvn::layout::TextureDesc {
        target: nxkit_nvn::layout::Target::D2,
        format: 0x06,
        flags: 0,
        samples: 0,
        width: 64,
        height: 64,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let id = g
        .with_front(|f| {
            let at = nxkit_nvn::space::GpuAddr::new(base + 0x1_0000)?;
            Ok(f.create_texture_shared_at(desc, at)?.0)
        })
        .expect("texture");
    let synced = |g: &Gpu| g.lock().resources_mut().synced(id).expect("synced");
    synced(&g);
    // a mapping in the range of the texture
    g.vpool_map(base, 0x1_0000, 0x1000, Some(0x1000_8000))
        .expect("map");
    assert_eq!(residency(&g, id), GuestNewer);
    let tables = g.vpool_snapshot().expect("tables");
    assert_eq!(
        tables.resolve(base + 0x1_0010),
        Some(0x10_0000_8010),
        "the page table holds the GPU address of the physical page"
    );
    synced(&g);
    // a mapping outside it
    g.vpool_map(base, 0, 0x1000, Some(0x1000_2000))
        .expect("map");
    assert_eq!(residency(&g, id), InSync);
    // a CPU write to the physical page behind it, and one to a page behind nothing
    g.guest_wrote_deferred(0x1000_9000, 0x10);
    assert_eq!(residency(&g, id), InSync);
    g.guest_wrote_deferred(0x1000_8800, 0x10);
    assert_eq!(residency(&g, id), GuestNewer);
    synced(&g);
    // an unmap
    g.vpool_map(base, 0x1_0000, 0x1000, None).expect("unmap");
    assert_eq!(residency(&g, id), GuestNewer);
    assert_eq!(
        g.vpool_snapshot().expect("tables").resolve(base + 0x1_0010),
        None
    );
    assert_eq!(
        tables.resolve(base + 0x1_0010),
        Some(0x10_0000_8010),
        "a snapshot stays"
    );
    // A request off the 4 KiB grid is refused.
    assert!(g.vpool_map(base, 0x800, 0x1000, Some(0x1000_2000)).is_err());
    g.vpool_finalize(pool);
    assert_eq!(g.vpool(pool), None);
    assert!(g.vpool_snapshot().is_none());
}

#[test]
fn a_virtual_pool_whose_range_never_left_the_host_becomes_an_alias() {
    let spy = Arc::new(Spy {
        window: Some((0x20_0000_0000, 0x100_0000)),
        ..Spy::default()
    });
    let g = gpu_with(spy);
    let base = g.vpool_init(0x5000, 0x1_0000).expect("a range");
    assert_eq!(g.vpool_decide(0x5000, true), Some(VMode::Alias));
    assert_eq!(
        g.vpool_take(0x5000),
        None,
        "an alias has no range of its own"
    );
    // the range went back: the next pool gets the one after it (the ring), and a request that does not keep the
    // offsets makes a pool sparse
    let next = g.vpool_init(0x6000, 0x1_0000).expect("a range");
    assert_eq!(next, base + 0x1_0000);
    assert_eq!(g.vpool_decide(0x6000, false), Some(VMode::Sparse));
    // no window: no range
    let g = gpu_with(Arc::new(Spy::default()));
    assert_eq!(g.vpool_init(0x5000, 0x1_0000), None);
    assert_eq!(g.vpool_decide(0x5000, true), None);
}

#[test]
fn a_cpu_write_after_a_mapping_loads_the_sparse_texture_again_at_the_next_flush() {
    use nxkit_nvn::resource::Residency::{GuestNewer, InSync};
    let phys = crate::nvn::abi::testing::alloc(0x1000);
    let spy = Arc::new(Spy {
        window: Some((0x20_0000_0000, 0x100_0000)),
        ..Spy::default()
    });
    let space = LinearSpace::new(phys, 0x10_0000_0000, 0x1000).expect("space");
    let g = Gpu::new(space, spy, Settings::default());
    let pool = 0x5000;
    let base = g.vpool_init(pool, 0x1_0000).expect("a range");
    g.vpool_take(pool);
    assert_eq!(g.vpool_decide(pool, true), Some(VMode::Sparse));
    let desc = nxkit_nvn::layout::TextureDesc {
        target: nxkit_nvn::layout::Target::D2,
        format: 0x06,
        flags: 0,
        samples: 0,
        width: 32,
        height: 32,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let id = g
        .with_front(|f| {
            let at = nxkit_nvn::space::GpuAddr::new(base)?;
            Ok(f.create_texture_shared_at(desc, at)?.0)
        })
        .expect("texture");
    let synced = |g: &Gpu| g.lock().resources_mut().synced(id).expect("synced");
    g.vpool_map(base, 0, 0x1000, Some(phys)).expect("map");
    assert_eq!(residency(&g, id), GuestNewer);
    // the host loads the texture while the page still holds zeros
    synced(&g);
    g.flush(FlushReason::CpuWait).expect("flush");
    assert_eq!(residency(&g, id), InSync, "the zeros did not change");
    // the game copies the mip into the physical page with the CPU, without a flush of the pool
    nxkit_abi::guest::fill(phys, 0x1000, 0x5A).expect("write");
    g.flush(FlushReason::CpuWait).expect("flush");
    assert_eq!(residency(&g, id), GuestNewer, "the flush saw the new bytes");
    synced(&g);
    g.flush(FlushReason::CpuWait).expect("flush");
    assert_eq!(residency(&g, id), InSync, "the same bytes load nothing");
    // an unmap ends the watch
    g.vpool_map(base, 0, 0x1000, None).expect("unmap");
    synced(&g);
    nxkit_abi::guest::fill(phys, 0x1000, 0x33).expect("write");
    g.flush(FlushReason::CpuWait).expect("flush");
    assert_eq!(residency(&g, id), InSync);
}
