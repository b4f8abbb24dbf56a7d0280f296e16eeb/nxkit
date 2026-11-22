//! Manual check of the GPU trace capture on a context as the host makes it. Run with
//! `MTL_CAPTURE_ENABLED=1 cargo test -p nxkit-metal --test gputrace_probe -- --ignored --nocapture`.

#[test]
#[ignore = "needs MTL_CAPTURE_ENABLED=1 and a GPU"]
fn a_capture_of_the_host_context_starts() {
    let Ok(ctx) = nxkit_metal::Context::system_default() else {
        return;
    };
    println!("supported {}", nxkit_metal::gpu_trace_supported());
    let path = std::env::temp_dir().join(format!("nxkit-probe-{}.gputrace", std::process::id()));
    let r = nxkit_metal::gpu_trace_start(&ctx, &path);
    println!("start {r:?}");
    nxkit_metal::gpu_trace_stop();
    let _ = std::fs::remove_dir_all(&path);
    assert!(r.is_ok(), "{r:?}");
}

#[test]
#[ignore = "needs MTL_CAPTURE_ENABLED=1 and a GPU"]
fn a_capture_after_a_collecting_pipeline_cache_starts() {
    let Ok(ctx) = nxkit_metal::Context::system_default() else {
        return;
    };
    let cfg = nxkit_metal::ArchiveConfig {
        collect: true,
        ..Default::default()
    };
    let cache = nxkit_metal::PipelineCache::new(&ctx, &cfg);
    println!("cache {}", cache.is_ok());
    let path = std::env::temp_dir().join(format!("nxkit-probe2-{}.gputrace", std::process::id()));
    let r = nxkit_metal::gpu_trace_start(&ctx, &path);
    println!("start with collect {r:?}");
    nxkit_metal::gpu_trace_stop();
    let _ = std::fs::remove_dir_all(&path);
}

fn probe(name: &str, ctx: &nxkit_metal::Context) {
    let path = std::env::temp_dir().join(format!(
        "nxkit-probe-{name}-{}.gputrace",
        std::process::id()
    ));
    let r = nxkit_metal::gpu_trace_start(ctx, &path);
    println!("PROBE {name}: {r:?}");
    nxkit_metal::gpu_trace_stop();
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
#[ignore = "needs MTL_CAPTURE_ENABLED=1 and a GPU"]
fn a_capture_after_the_host_parts_starts() {
    let Ok(ctx) = nxkit_metal::Context::system_default() else {
        return;
    };
    let guest = nxkit_metal::GuestMemory::allocate(&ctx, 1 << 30);
    println!("guest {}", guest.is_ok());
    probe("guest", &ctx);
    let mirror = nxkit_metal::Mirror::new(&ctx, 1 << 30);
    println!("mirror {}", mirror.is_ok());
    probe("mirror", &ctx);
    let ring = nxkit_metal::FrameRing::new(&ctx);
    println!("ring {}", ring.is_ok());
    probe("ring", &ctx);
    let up = nxkit_metal::PresentUpscaler::new(&ctx, nxkit_metal::PresentUpscale::MetalFx);
    let _ = &up;
    probe("metalfx", &ctx);
}
