//! Tests for the guest address space of the backend.

use super::*;
use nxkit_metal::{GUEST_VA_BASE, GUEST_VA_MAX, is_guest_va};

fn backend(len: u64) -> Option<(Arc<GuestMemory>, Arc<GpuBackend>)> {
    let ctx = match Context::system_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            return None;
        }
    };
    let guest = Arc::new(GuestMemory::allocate(&ctx, len).unwrap());
    let backend = GpuBackend::new(&ctx, guest.clone(), &GpuSettings::default()).unwrap();
    Some((guest, backend))
}

#[test]
fn the_gpu_base_is_the_virtual_base() {
    let Some((guest, backend)) = backend(1 << 20) else {
        return;
    };
    assert_eq!(Backend::gpu_base(&*backend), GUEST_VA_BASE);
    // The buffer has a real address of its own, and the game never sees it.
    assert_ne!(guest.real_base(), GUEST_VA_BASE);
    assert!(!is_guest_va(guest.real_base()));
    assert_eq!(
        Backend::host_range(&*backend),
        Some((guest.host_base(), guest.len()))
    );
}

#[test]
fn a_pool_resolves_from_its_virtual_address_to_the_real_one() {
    let Some((guest, backend)) = backend(1 << 20) else {
        return;
    };
    let pool = PoolInfo {
        host: guest.host_base() + 0x4000,
        len: 0x8000,
        flags: 0,
    };
    backend.register_pool(pool).unwrap();
    let snap = backend.pools.current();
    let guest_addr = GUEST_VA_BASE + 0x4000 + 0x123;
    let loc = snap.resolve(guest_addr, 4).unwrap();
    assert_eq!(loc.real, guest.real_base() + 0x4000 + 0x123);
    assert!(!is_guest_va(loc.real));
    // The real address is not a guest address: it resolves to nothing.
    assert!(snap.resolve(loc.real, 4).is_err());
    // The window ends where the guest range does not reach.
    assert!(guest_addr + guest.len() < GUEST_VA_MAX);
}

#[test]
fn the_space_of_the_backend_maps_host_addresses_to_virtual_ones() {
    let Some((guest, backend)) = backend(1 << 20) else {
        return;
    };
    let map = backend.guest.address_map();
    assert_eq!(map.gpu_base(), GUEST_VA_BASE);
    assert_eq!(
        map.gpu_address(guest.host_base() + 0x40, 1),
        Ok(GUEST_VA_BASE + 0x40)
    );
}

#[test]
fn with_the_mirror_a_fence_wait_returns_at_the_flush_and_the_gpu_is_not_asked() {
    let _t = crate::private::TEST_TURN
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    crate::private::test_mode(Some(crate::private::Mode::Mirror));
    let Some((_guest, backend)) = backend(64 << 20) else {
        crate::private::test_mode(None);
        return;
    };
    // Fence 1 (inside the run-ahead of one frame) is flushed and captured; the GPU has not seen it and nothing was encoded.
    backend.flushed.store(1, Ordering::Release);
    backend.captured.store(1, Ordering::Release);
    let t0 = std::time::Instant::now();
    assert_eq!(
        Backend::wait_fence(&*backend, Fence(1), u64::MAX),
        WaitResult::Done
    );
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "no wait for the GPU"
    );
    assert_eq!(
        backend.done.load(Ordering::Acquire),
        0,
        "the GPU is not done with it, and nothing says so"
    );
    // A fence that is done answers as before.
    backend.done.store(3, Ordering::Release);
    assert_eq!(
        Backend::wait_fence(&*backend, Fence(2), u64::MAX),
        WaitResult::AlreadyDone
    );
    crate::private::test_mode(None);
}

#[test]
fn a_pool_call_with_the_encoder_lock_free_reaches_the_encoder_at_once() {
    let Some((guest, backend)) = backend(1 << 20) else {
        return;
    };
    let host = guest.host_base() + 0x4000;
    backend
        .register_pool(PoolInfo {
            host,
            len: 0x8000,
            flags: 0,
        })
        .unwrap();
    // Read without draining the queue: the op was applied by the call itself.
    assert_eq!(
        locked(&backend.gpu).pools(),
        [(GUEST_VA_BASE + 0x4000, 0x8000)]
    );
    backend.unregister_pool(host);
    assert!(locked(&backend.gpu).pools().is_empty());
    assert_eq!(backend.pool_ops.counts(), (2, 0));
}

#[test]
fn pool_calls_during_an_encode_are_applied_in_order_at_the_next_lock() {
    let Some((guest, backend)) = backend(1 << 20) else {
        return;
    };
    let host = guest.host_base() + 0x4000;
    let pool = PoolInfo {
        host,
        len: 0x8000,
        flags: 0,
    };
    {
        // An encode holds the lock: the calls return without it.
        let held = locked(&backend.gpu);
        backend.register_pool(pool).unwrap();
        backend.unregister_pool(host);
        backend.register_pool(pool).unwrap();
        assert!(held.pools().is_empty());
        // The snapshot of the next flush has the pool at once.
        assert!(
            backend
                .pools
                .current()
                .resolve(GUEST_VA_BASE + 0x4000, 4)
                .is_ok()
        );
    }
    assert_eq!(backend.pool_ops.counts(), (0, 3));
    // Register, unregister, register: one pool is left, not none.
    assert_eq!(
        backend.lock_gpu().pools(),
        [(GUEST_VA_BASE + 0x4000, 0x8000)]
    );
}

#[test]
fn a_pool_outside_guest_memory_fails_at_the_call_even_while_an_encode_runs() {
    let Some((guest, backend)) = backend(1 << 20) else {
        return;
    };
    let bad = PoolInfo {
        host: guest.host_base() + guest.len() - 0x1000,
        len: 0x8000,
        flags: 0,
    };
    {
        let _held = locked(&backend.gpu);
        assert!(backend.register_pool(bad).is_err());
    }
    assert!(backend.register_pool(bad).is_err());
    assert_eq!(backend.pool_ops.counts(), (0, 0));
    assert!(backend.lock_gpu().pools().is_empty());
}

#[test]
fn the_event_signals_of_a_frame_reach_the_counter_word_and_not_the_counter_reports() {
    use crate::detail::{CounterReport, REPORT_EVENT_ADD, REPORT_EVENT_WRITE};
    let Some((guest, backend)) = backend(1 << 20) else {
        return;
    };
    let word = guest.host_base() + 0x40;
    let counter = |off: u64| {
        let mut b = [0u8; 4];
        guest.read(off, &mut b).unwrap();
        u32::from_le_bytes(b)
    };
    let detail = FrameDetail {
        reports: vec![
            CounterReport {
                kind: REPORT_EVENT_WRITE,
                value: 5,
                addr: word,
            },
            CounterReport {
                kind: REPORT_EVENT_ADD,
                value: 3,
                addr: word,
            },
            CounterReport {
                kind: crate::detail::REPORT_VALUE,
                value: 1,
                addr: GUEST_VA_BASE + 0x100,
            },
        ],
        ..FrameDetail::default()
    };
    // A frame without a batch is complete at once.
    backend.events.hold(1, &detail.reports, None);
    backend.hold_counters(
        Fence(1),
        &detail,
        &Ok::<EncodeReport, ()>(EncodeReport::default()),
    );
    assert_eq!(counter(0x40), 8);
    let held = locked(&backend.counters).clone();
    assert_eq!(held.len(), 1, "only the value report waits for the GPU");
    assert_eq!(held[0].1.kind, crate::detail::REPORT_VALUE);
}

#[test]
fn a_deferred_submit_walks_with_the_device_origin_captured_before_a_device_mode_change() {
    let Some((_guest, backend)) = backend(1 << 20) else {
        return;
    };
    backend.device_modes(true, false);
    let first = backend.submit(1, &[], false).unwrap();
    backend.device_modes(false, false);
    let second = backend.submit(2, &[], true).unwrap();
    backend.device_modes(true, false);
    // Neither walk runs until after the mode changed, as with a held submit or the helper thread.
    second();
    first();
    let walker = locked(&backend.walker);
    assert_eq!(walker.origins(1), Some([true, false]));
    assert_eq!(walker.origins(2), Some([false, true]));
}

/// Drive the production recorder, decoder, walker, resource store and encoder.
/// The only fixture dependencies are synthetic shaders and input byte vectors.
#[test]
#[ignore = "requires Metal 4 and the precompiled synthetic tessellation fixture"]
fn tessellation_replay_walk_and_readback() {
    use nxkit_nvn::{
        backend::{FlushCx, ShaderKey, StageKey},
        ext::{DrawKind, Ext, ExtCode, Setting, StateKind},
        frame::MAX_COLOR,
        front::{FlushReason, FrontEnd},
        layout::{Target, TextureDesc},
        record::Record,
        space::LinearSpace,
    };
    assert!(crate::tess_replay::enabled(), "set NXKIT_TESS_REPLAY=1");
    let dir = std::path::PathBuf::from(
        std::env::var_os("NXKIT_TESS_FIXTURE_DIR")
            .expect("set NXKIT_TESS_FIXTURE_DIR to the synthetic fixture root"),
    );
    let dir = dir
        .canonicalize()
        .expect("cannot resolve synthetic fixture root");
    for key in [
        "0101010101010101_b",
        "0101010101010101_bt",
        "0202020202020202_b",
        "0303030303030303_b",
    ] {
        for extension in ["json", "metallib"] {
            let path = dir.join("shaders").join(format!("{key}.{extension}"));
            assert!(
                path.is_file(),
                "missing required fixture {}",
                path.display()
            );
        }
    }
    let ctx = Context::system_default().unwrap();
    let guest = Arc::new(GuestMemory::allocate(&ctx, 1 << 20).unwrap());
    let gpu = GpuBackend::new(
        &ctx,
        guest.clone(),
        &GpuSettings {
            shader_dir: dir.join("shaders"),
            scale: 1.0,
            threads: 1,
            ..Default::default()
        },
    )
    .unwrap();
    // The fixture selects pixels in top-left viewport coordinates.
    Backend::device_modes(&*gpu, false, false);
    let h = guest.host_base();
    let g = Backend::gpu_base(&*gpu);
    Backend::register_pool(
        &*gpu,
        PoolInfo {
            host: h,
            len: 1 << 20,
            flags: 0,
        },
    )
    .unwrap();
    let mut front = FrontEnd::new(
        LinearSpace::new(h, g, 1 << 20).unwrap(),
        GuestView(gpu.guest.clone()),
    );
    let vector = |name| std::fs::read(dir.join("vectors").join(name)).unwrap();
    let vertices = vector("vertices.bin");
    assert_eq!(vertices.len(), 224);
    guest.write(0x4000, &vertices).unwrap();
    guest.write(0x4400, &vertices).unwrap();
    guest
        .write(0x5000, &vector("domain_uniform_slot2.bin"))
        .unwrap();
    let target = front
        .create_texture(
            TextureDesc {
                target: Target::D2,
                format: 0x25,
                flags: 0,
                samples: 0,
                width: 64,
                height: 64,
                depth: 1,
                levels: 1,
                stride: 0,
                packaged_block: None,
            },
            h + 0x8000,
        )
        .unwrap();
    Backend::texture_created(&*gpu, front.resources().get(target).unwrap());
    for id in 1..=3 {
        Backend::program_set(
            &*gpu,
            u64::from(id),
            &[StageKey {
                index: 0,
                key: Some(ShaderKey([id; 8])),
                code: 0,
                code_len: 0,
            }],
        );
    }
    let mut color = [0; MAX_COLOR];
    color[0] = front.texture_address(target).unwrap();
    // The same indexed draw-only recording is evaluated against two GPU writes of different bytes.
    // The second call must hit the nested walker template but must still consume the live index buffer.
    let indexed_child = 16;
    front
        .add_command_memory(indexed_child, h + 0x30000, 0x1000)
        .unwrap();
    front.begin_recording(indexed_child).unwrap();
    front
        .write(
            indexed_child,
            Record::Ext(
                Ext::new(ExtCode::Draw, DrawKind::Elements as u16)
                    .u32(crate::detail::PATCHES)
                    .u32(1)
                    .u32(6)
                    .u64(g + 0x7002)
                    .i32(-2)
                    .u32(0)
                    .u32(1),
            ),
        )
        .unwrap();
    let indexed_child = front.end_recording(indexed_child).unwrap();
    let modes = [
        (None, 1., [255, 0, 80, 255], [0, 255, 112, 255]),
        (None, 2.5, [255, 0, 104, 255], [0, 255, 135, 255]),
        (None, 0., [0; 4], [0; 4]),
        (
            Some("indexed.bin"),
            1.,
            [255, 0, 112, 255],
            [0, 255, 80, 255],
        ),
        (
            Some("indexed_invalid_right.bin"),
            1.,
            [255, 0, 112, 255],
            [0; 4],
        ),
    ];
    for (number, (file, factor, left, right)) in modes.into_iter().enumerate() {
        let base = h + 0x20000 + number as u64 * 0x1000;
        // Distinct command ranges ensure no previous recording supplies stale state.
        let cb = number;
        front.add_command_memory(cb, base, 0x1000).unwrap();
        front.begin_recording(cb).unwrap();
        front
            .write(cb, Record::SetTargets { color, depth: 0 })
            .unwrap();
        let mut ext = |r| front.write(cb, Record::Ext(r)).unwrap();
        let set = |s| Ext::new(ExtCode::Setting, s as u16);
        ext(Ext::new(ExtCode::ClearColor, 0)
            .u32(15)
            .u32(0)
            .u32(0)
            .u32(0)
            .u32(0));
        ext(Ext::new(ExtCode::Viewport, 0)
            .f32(0.)
            .f32(0.)
            .f32(64.)
            .f32(64.));
        ext(Ext::new(ExtCode::Scissor, 0).i32(0).i32(0).i32(64).i32(64));
        // NVN polygon defaults: no cull, counter-clockwise front, and filled triangles.
        // An unbound polygon state uses mode zero, which the Metal backend draws as lines.
        ext(Ext::new(ExtCode::State, StateKind::Polygon as u16).u32((1 << 2) | (2 << 3)));
        for (program, mask) in [(1, 1), (2, 2), (3, 16)] {
            ext(Ext::new(ExtCode::Program, 0).u64(program).u32(mask));
        }
        ext(Ext::new(ExtCode::VertexBuffer, 0).u64(g + 0x4000).u64(224));
        ext(Ext::new(ExtCode::VertexState, 0)
            .u32(0)
            .u32(2)
            .u32(0x2e)
            .u32(0x102e));
        ext(Ext::new(ExtCode::VertexState, 1)
            .u32(0)
            .u32(1)
            .u32(32)
            .u32(0));
        ext(Ext::new(ExtCode::UniformBuffer, 0x402)
            .u64(g + 0x5000)
            .u64(16));
        ext(set(Setting::PatchSize).u32(3));
        ext(set(Setting::InnerTessellationLevels)
            .f32(factor)
            .f32(factor));
        ext(set(Setting::OuterTessellationLevels)
            .f32(factor)
            .f32(factor)
            .f32(factor)
            .f32(factor));
        if let Some(file) = file {
            let mut bytes = vector(file);
            bytes.resize(16, 0);
            let source = 0x6000 + number as u64 * 32;
            guest.write(source, &bytes).unwrap();
            guest.write(0x7000, &[0xFF; 16]).unwrap();
            ext(Ext::new(ExtCode::CopyBufferToBuffer, 0)
                .u64(g + source)
                .u64(g + 0x7000)
                .u64(16)
                .u32(0));
            front.write(cb, Record::Call(indexed_child.0)).unwrap();
        } else {
            if number == 1 {
                // Empty destination on the CPU; only GPU visibility can restore the correct positions and tints.
                guest.write(0x4000, &[0; 224]).unwrap();
                ext(Ext::new(ExtCode::CopyBufferToBuffer, 0)
                    .u64(g + 0x4400)
                    .u64(g + 0x4000)
                    .u64(224)
                    .u32(0));
            }
            ext(Ext::new(ExtCode::Draw, DrawKind::Arrays as u16)
                .u32(crate::detail::PATCHES)
                .u32(1)
                .u32(6));
        }
        // Keep the attachment live through the end of the pass.
        front.write(cb, Record::CpuRead(color[0])).unwrap();
        let recording = front.end_recording(cb).unwrap();
        Backend::submit(&*gpu, 0, &[recording], false).unwrap()();
        front.submit(recording).unwrap();
        front.end_frame(FlushReason::CpuWait).unwrap();
        let (frame, plan, table, descriptors) = front.flush_parts();
        Backend::flush_frame(
            &*gpu,
            frame,
            plan,
            FlushReason::CpuWait,
            Fence(number as u64 + 1),
            FlushCx { table, descriptors },
        )
        .unwrap();
        if let Some(enc) = &gpu.enc {
            let settled = enc.settle();
            assert!(
                settled.error.is_none(),
                "encoder failed: {:?}",
                settled.error
            );
            if let Some(update) = settled.update {
                update.apply(table);
            }
        }
        let command = locked(&gpu.test_submitted)
            .take()
            .expect("no Metal command buffer");
        let completed = command.wait().expect("Metal command buffer failed");
        assert!(
            !completed.failed,
            "case {number}: Metal reported failure: {completed:?}"
        );
        Backend::wait_idle(&*gpu);
        let stats = locked(&gpu.test_encode_stats)
            .take()
            .expect("no encoder report");
        assert_eq!(stats.skipped_total(), 0, "case {number}: {stats:?}");
        assert_eq!(stats.draws, 1, "case {number}: {stats:?}");
        // The guest draw counter includes an intentionally culled patch. Capture does not.
        assert_eq!(
            stats.kinds.metal_draws,
            u32::from(factor != 0.0),
            "case {number}: unexpectedly missing or extra capture: {stats:?}"
        );
        let walk = locked(&gpu.test_walk_counters)
            .take()
            .expect("no walker report");
        assert_eq!(walk.draws, 1, "case {number}: {walk:?}");
        assert_eq!(
            walk.walk_hits,
            u32::from(number == 4),
            "case {number}: {walk:?}"
        );
        Backend::settle(&*gpu, table);
        let output = locked(&gpu.gpu)
            .texture(target)
            .expect("target was not rendered");
        let got = output
            .debug_read(&ctx, &[[10, 40], [42, 40], [30, 10]])
            .expect("Metal texture readback failed");
        assert_eq!(got.len(), 3, "missing selected pixels for case {number}");
        let want = [left, right, [0; 4]];
        if !got
            .iter()
            .zip(want)
            .all(|(pixel, want)| pixel.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 1))
        {
            // Sparse coverage separates an empty target from geometry outside the selected pixels.
            let probes: Vec<_> = (2..64)
                .step_by(4)
                .flat_map(|y| (2..64).step_by(4).map(move |x| [x, y]))
                .collect();
            let coverage = output.debug_read(&ctx, &probes).map(|pixels| {
                probes
                    .iter()
                    .zip(pixels)
                    .filter(|(_, pixel)| *pixel != [0; 4])
                    .map(|(point, pixel)| (*point, pixel))
                    .collect::<Vec<_>>()
            });
            panic!(
                "case {number}: got {got:?}, expected {want:?}; \
                 sampled coverage {coverage:?}; attachments {:?}",
                plan.attachments
            );
        }
        front.begin_frame();
    }
}
