use super::*;

#[test]
fn a_range_is_in_a_pool_only_when_the_pool_holds_all_of_it() {
    let mut c = CopyBack::default();
    c.add_pool(0x1_0000, 0x1_0000, 0x4);
    c.add_pool(0x3_0000, 0x1_0000, CPU_NO_ACCESS);
    assert_eq!(c.pool_flags(0x1_0000, 0x2_0000), Some(0x4));
    assert_eq!(c.pool_flags(0x1_8000, 0x1_9000), Some(0x4));
    assert_eq!(c.pool_flags(0x1_8000, 0x2_0001), None);
    assert_eq!(c.pool_flags(0x0_8000, 0x0_9000), None);
    assert_eq!(c.pool_flags(0x2_0000, 0x2_1000), None);
    assert_eq!(c.pool_flags(0x3_0000, 0x3_1000), Some(CPU_NO_ACCESS));
}

#[test]
fn a_pool_inside_another_does_not_remove_it() {
    let mut c = CopyBack::default();
    c.add_pool(0x1_0000, 0x4_0000, 0x4);
    c.add_pool(0x2_0000, 0x1_0000, CPU_NO_ACCESS);
    assert_eq!(c.pool_of(0x1_1000), Some((0x1_0000, 0x5_0000, 0x4)));
    assert_eq!(
        c.pool_of(0x2_8000),
        Some((0x2_0000, 0x3_0000, CPU_NO_ACCESS))
    );
    // a range that the inner pool does not hold whole belongs to the outer one
    assert_eq!(c.pool_flags(0x2_8000, 0x3_8000), Some(0x4));
    c.remove_pool(0x2_0000);
    assert_eq!(c.pool_of(0x2_8000), Some((0x1_0000, 0x5_0000, 0x4)));
    assert_eq!(c.pool_flags(0x2_8000, 0x2_9000), Some(0x4));
}

#[test]
fn a_new_pool_at_the_start_of_an_old_one_survives_the_removal_of_the_old_one() {
    let mut c = CopyBack::default();
    c.add_pool(0x1_0000, 0x1_0000, 0x4);
    c.add_pool(0x1_0000, 0x2_0000, CPU_NO_ACCESS);
    c.remove_pool(0x1_0000);
    assert_eq!(
        c.pool_of(0x2_8000),
        Some((0x1_0000, 0x3_0000, CPU_NO_ACCESS))
    );
    c.remove_pool(0x1_0000);
    assert_eq!(c.pool_of(0x2_8000), None);
}

#[test]
fn pool_of_gives_the_newest_pool_for_an_address_in_two_pools() {
    let mut c = CopyBack::default();
    c.add_pool(0x1_0000, 0x1_0000, 0x4);
    c.add_pool(0x1_8000, 0x1_0000, CPU_NO_ACCESS);
    assert_eq!(c.pool_of(0x1_1000), Some((0x1_0000, 0x2_0000, 0x4)));
    assert_eq!(
        c.pool_of(0x1_9000),
        Some((0x1_8000, 0x2_8000, CPU_NO_ACCESS))
    );
    assert_eq!(c.pool_flags(0x1_8000, 0x1_9000), Some(CPU_NO_ACCESS));
    // the older pool holds the whole range that the newer one does not
    assert_eq!(c.pool_flags(0x1_0000, 0x1_1000), Some(0x4));
    c.remove_pool(0x1_8000);
    assert_eq!(c.pool_of(0x1_9000), Some((0x1_0000, 0x2_0000, 0x4)));
    assert_eq!(c.pool_of(0x2_1000), None);
}

#[test]
fn an_upload_waits_only_for_recent_copy_backs_it_overlaps() {
    use nxkit_nvn::layout::{Target, TextureDesc};
    use nxkit_nvn::resource::ResourceTable;
    use nxkit_nvn::space::GpuAddr;
    let desc = TextureDesc {
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
    };
    let mut t = ResourceTable::new();
    let id = t.create(desc, GpuAddr::new(0x10_0000).unwrap()).unwrap();
    let rec = *t.get(id).unwrap();
    let mut c = CopyBack::default();
    assert!(!c.upload_waits(&rec));
    c.submitted(1, &[(rec.end(), rec.end() + 0x1000)]);
    assert!(!c.upload_waits(&rec));
    c.submitted(2, &[(rec.address.get() + 0x100, rec.address.get() + 0x200)]);
    assert!(c.upload_waits(&rec));
    c.submitted(2 + RECENT_BATCHES, &[]);
    assert!(!c.upload_waits(&rec));
    c.submitted(20, &[(rec.address.get(), rec.end())]);
    c.idle();
    assert!(!c.upload_waits(&rec));
}

/// A 64x64 RGBA8 texture at `addr`: 0x4000 bytes of storage in a pitch or block layout.
fn texture(addr: u64) -> (TexId, TextureRecord) {
    texture_with(addr, 0)
}

/// The same with the `NVNtextureFlags` `flags`.
fn texture_with(addr: u64, flags: u32) -> (TexId, TextureRecord) {
    use nxkit_nvn::layout::{Target, TextureDesc};
    use nxkit_nvn::resource::ResourceTable;
    use nxkit_nvn::space::GpuAddr;
    let desc = TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags,
        samples: 0,
        width: 64,
        height: 64,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let mut t = ResourceTable::new();
    let id = t.create(desc, GpuAddr::new(addr).unwrap()).unwrap();
    (id, *t.get(id).unwrap())
}

const PHYS_FLAGS: u32 = 0x18c;

#[test]
fn a_window_with_cpu_no_access_hides_a_texture_of_a_cpu_visible_physical_pool() {
    let (id, rec) = texture(0x10_0000);
    let mut c = CopyBack::default();
    c.add_pool(0x10_0000, 0x10_0000, PHYS_FLAGS);
    assert_eq!(c.why(id, &rec), (Why::Copied, Some(PHYS_FLAGS)));
    assert_eq!(c.candidate(id, &rec), Some(PHYS_FLAGS));
    c.add_window(0x10_0000, 0x10_0000, CPU_NO_ACCESS);
    assert_eq!(
        c.why(id, &rec),
        (Why::WindowNoCpu, Some(CPU_NO_ACCESS | VIRTUAL))
    );
    assert_eq!(c.candidate(id, &rec), None);
    c.remove_windows(0x10_0000, 0x10_0000);
    assert_eq!(c.candidate(id, &rec), Some(PHYS_FLAGS));
}

#[test]
fn an_unmap_drops_only_the_windows_inside_it_and_never_a_physical_pool() {
    let (id, rec) = texture(0x10_0000);
    let mut c = CopyBack::default();
    c.add_pool(0x10_0000, 0x10_0000, PHYS_FLAGS);
    c.add_window(0x10_0000, 0x4000, CPU_NO_ACCESS);
    c.add_window(0x20_0000 - 0x4000, 0x4000, CPU_NO_ACCESS);
    // a partial unmap leaves the window
    c.remove_windows(0x10_0000, 0x2000);
    assert_eq!(c.candidate(id, &rec), None);
    c.remove_windows(0x10_0000, 0x4000);
    assert_eq!(c.candidate(id, &rec), Some(PHYS_FLAGS));
    assert_eq!(
        c.pool_of(0x10_0000),
        Some((0x10_0000, 0x20_0000, PHYS_FLAGS))
    );
    // the same range mapped twice is one window
    c.add_window(0x10_0000, 0x4000, CPU_NO_ACCESS);
    c.add_window(0x10_0000, 0x4000, CPU_NO_ACCESS);
    c.remove_windows(0x10_0000, 0x4000);
    assert_eq!(c.candidate(id, &rec), Some(PHYS_FLAGS));
}

#[test]
fn a_texture_over_two_hidden_windows_is_hidden_and_over_a_visible_one_is_not() {
    let (id, rec) = texture(0x10_0000);
    let mut c = CopyBack::default();
    c.add_pool(0x10_0000, 0x10_0000, PHYS_FLAGS);
    c.add_window(0x10_0000, 0x2000, CPU_NO_ACCESS);
    // half covered: the physical pool decides
    assert_eq!(c.why(id, &rec).0, Why::Copied);
    c.add_window(0x10_2000, 0x2000, CPU_NO_ACCESS);
    assert_eq!(c.why(id, &rec).0, Why::WindowNoCpu);
    // a window the CPU may read, over part of it
    c.add_window(0x10_3000, 0x1000, 0);
    assert_eq!(c.why(id, &rec).0, Why::Copied);
}

#[test]
fn gpu_no_access_of_the_physical_pool_alone_does_not_reject() {
    let (id, rec) = texture(0x10_0000);
    let mut c = CopyBack::default();
    c.add_pool(0x10_0000, 0x10_0000, 0x8 | 0x4);
    assert_eq!(c.why(id, &rec).0, Why::Copied);
    c.add_pool(0x10_0000, 0x10_0000, 0x8 | CPU_NO_ACCESS);
    assert_eq!(c.why(id, &rec).0, Why::PoolNoCpu);
}

#[test]
fn a_compressible_texture_in_compressible_memory_is_not_copied_back() {
    use nxkit_nvn::layout::FLAG_COMPRESSIBLE;
    let (id, rec) = texture_with(0x10_0000, FLAG_COMPRESSIBLE);
    let mut c = CopyBack::default();
    // Octopath Traveler's pool: CPU_UNCACHED | GPU_CACHED | SHADER_CODE | COMPRESSIBLE
    c.add_pool(0x10_0000, 0x10_0000, 0xe2);
    assert_eq!(c.why(id, &rec), (Why::Compressed, Some(0xe2)));
    assert_eq!(c.candidate(id, &rec), None);
    // the same texture without the flag, or in a pool without compression, is read as it is
    let (plain, plain_rec) = texture(0x10_0000);
    assert_eq!(c.why(plain, &plain_rec).0, Why::Copied);
    let mut d = CopyBack::default();
    d.add_pool(0x10_0000, 0x10_0000, 0x62);
    assert_eq!(d.why(id, &rec).0, Why::Copied);
    // a window the CPU may read over a compressible physical pool: the physical pool compresses
    d.add_pool(0x10_0000, 0x10_0000, PHYS_FLAGS);
    d.add_window(0x10_0000, 0x10_0000, 0x4);
    assert_eq!(d.why(id, &rec).0, Why::Compressed);
}

#[test]
fn a_texture_in_no_pool_or_with_another_reason_says_so() {
    let (id, rec) = texture(0x10_0000);
    let mut c = CopyBack::default();
    assert_eq!(c.why(id, &rec), (Why::NoPool, None));
    c.add_pool(0x10_0000, 0x10_0000, 0);
    c.presented.insert(id);
    assert_eq!(c.why(id, &rec).0, Why::Presented);
    let mut frame_local = rec;
    frame_local.frame_local = true;
    assert_eq!(c.why(id, &frame_local).0, Why::FrameLocal);
}

#[test]
fn the_registry_tells_overlap_and_coverage() {
    let mut r: Registry<u32> = Registry::default();
    r.add_with(0x1000, 0x1000, 1);
    r.add_with(0x2000, 0x1000, 1);
    r.add_with(0x3000, 0x1000, 2);
    assert!(r.covered_by(0x1000, 0x3000, |v| v == 1));
    assert!(!r.covered_by(0x1000, 0x3001, |v| v == 1));
    assert!(!r.covered_by(0x0fff, 0x2000, |v| v == 1));
    assert!(r.any_overlap(0x2fff, 0x3001, |v| v == 2));
    assert!(!r.any_overlap(0x1000, 0x3000, |v| v == 2));
    assert_eq!(r.remove_where(|_, _, v| v == 1), 2);
    assert_eq!(r.len(), 1);
}

#[test]
fn dirty_parts_stay_inside_the_texture() {
    let mut d = Dirty::default();
    assert!(d.is_empty());
    d.add(1, 3);
    d.add(0, 3);
    d.add(1, 3);
    d.add(9, 0);
    assert_eq!(d.parts(2, 8), vec![(0, 3), (1, 3)]);
    assert_eq!(Dirty::All.parts(2, 2), vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
}
