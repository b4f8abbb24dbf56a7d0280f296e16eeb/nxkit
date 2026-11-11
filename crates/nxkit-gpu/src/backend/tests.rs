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
