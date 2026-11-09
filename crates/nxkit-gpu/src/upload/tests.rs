//! Tests for the upload and copy-back paths.

use super::*;
use nxkit_metal::{
    ArchiveConfig, ArgumentTable, DataBuffer, Dependency, FrameRing, ShaderLibrary, Use,
};
use nxkit_nvn::{resource::ResourceTable, space::GpuAddr};

/// The snapshot of the whole mode over `guest`: one pool at the virtual base. The game's addresses are virtual, so a
/// site that does not resolve them through the snapshot misses the pool and the test fails.
fn whole(guest: &Arc<GuestMemory>) -> GuestPools {
    GuestPools::whole(nxkit_metal::GUEST_VA_BASE, guest.len(), guest.clone())
}

/// Uploads `desc` from a pattern in guest memory, clears the memory, copies `dirty` back and returns the
/// pattern, the guest bytes after the copy-back and the record.
fn round_trip(
    desc: nxkit_nvn::layout::TextureDesc,
    dirty: &Dirty,
) -> Option<(Vec<u8>, Vec<u8>, TextureRecord)> {
    let ctx = match Context::system_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            return None;
        }
    };
    let guest = Arc::new(GuestMemory::allocate(&ctx, 1024 * 1024).unwrap());
    let pools = whole(&guest);
    let mut resources = ResourceTable::new();
    let id = resources
        .create(
            desc,
            GpuAddr::new(nxkit_metal::GUEST_VA_BASE + 0x1000).unwrap(),
        )
        .unwrap();
    let rec = *resources.get(id).unwrap();
    let size = rec.layout.size as usize;
    let pattern: Vec<u8> = (0..size).map(|i| (i * 7 + i / 251) as u8 | 1).collect();
    guest.write(0x1000, &pattern).unwrap();
    let mut store = crate::textures::TextureStore::new(&ctx);
    let res = store
        .ensure(
            &rec,
            1.0,
            crate::textures::Use::default(),
            &mut Default::default(),
        )
        .unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let transfer = Transfer::new(&ctx, &cache, GobOrder::Tegra).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    transfer
        .upload(frame.commands(), &scratch, &pools, &rec, &res, false)
        .unwrap();
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    guest.write(0x1000, &vec![0; size]).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let results =
        transfer.copy_back_many(frame.commands(), &scratch, &pools, &[(&rec, &res, dirty)]);
    assert!(results[0].is_ok(), "{results:?}");
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let mut out = vec![0; size];
    guest.read(0x1000, &mut out).unwrap();
    Some((pattern, out, rec))
}

#[test]
fn copy_back_writes_the_dirty_levels_and_layers_of_a_block_linear_array() {
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::D2Array,
        format: 0x06,
        flags: 0,
        samples: 0,
        width: 64,
        height: 64,
        depth: 3,
        levels: 2,
        stride: 0,
        packaged_block: None,
    };
    let mut dirty = Dirty::default();
    dirty.add(0, 0);
    dirty.add(1, 2);
    let Some((pattern, out, rec)) = round_trip(desc, &dirty) else {
        return;
    };
    // Both levels fill whole GOB blocks, so every byte of a level is a texel.
    let level = |l: u32| {
        let lo = desc.level_offset(l).unwrap();
        let len = (64u64 >> l) * (64u64 >> l) * 2;
        (lo, len)
    };
    for slice in 0..3u64 {
        for l in 0..2 {
            let (lo, len) = level(l);
            let at = (slice * rec.layout.layer_size + lo) as usize;
            let got = &out[at..at + len as usize];
            if dirty.parts(2, 3).contains(&(l, slice as u32)) {
                assert_eq!(
                    got,
                    &pattern[at..at + len as usize],
                    "level {l} slice {slice}"
                );
            } else {
                assert!(
                    got.iter().all(|b| *b == 0),
                    "level {l} slice {slice} was written"
                );
            }
        }
    }
}

#[test]
fn copy_back_writes_the_rows_of_a_pitch_linear_texture() {
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags: nxkit_nvn::layout::FLAG_LINEAR_RENDER_TARGET,
        samples: 0,
        width: 20,
        height: 8,
        depth: 1,
        levels: 1,
        stride: 128,
        packaged_block: None,
    };
    let Some((pattern, out, _)) = round_trip(desc, &Dirty::All) else {
        return;
    };
    for row in 0..8 {
        let at = row * 128;
        assert_eq!(&out[at..at + 80], &pattern[at..at + 80], "row {row}");
        assert!(
            out[at + 80..at + 128].iter().all(|b| *b == 0),
            "the padding of row {row} was written"
        );
    }
}

#[test]
fn compressed_upload_preserves_every_mip_and_array_layer() {
    let ctx = match Context::system_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            return;
        }
    };
    let guest = Arc::new(GuestMemory::allocate(&ctx, 1024 * 1024).unwrap());
    let pools = whole(&guest);
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::D2Array,
        format: 0x41,
        flags: 0,
        samples: 0,
        width: 20,
        height: 12,
        depth: 2,
        levels: 5,
        stride: 0,
        packaged_block: None,
    };
    let mut resources = ResourceTable::new();
    let id = resources
        .create(desc, GpuAddr::new(nxkit_metal::GUEST_VA_BASE).unwrap())
        .unwrap();
    let rec = resources.get(id).unwrap();
    // Every block in a level has a constant BC1 colour, with a distinct colour in each level and layer.
    for slice in 0u32..2 {
        for level in 0..5 {
            let color = if slice == 0 {
                (31u16 - level as u16) << 11
            } else {
                (63u16 - level as u16) << 5
            };
            let block = [color as u8, (color >> 8) as u8, 0, 0, 0, 0, 0, 0];
            let lo = desc.level_offset(level).unwrap();
            let hi = desc.level_offset(level + 1).unwrap();
            let bytes: Vec<u8> = block.into_iter().cycle().take((hi - lo) as usize).collect();
            guest
                .write(u64::from(slice) * rec.layout.layer_size + lo, &bytes)
                .unwrap();
        }
    }
    let mut store = crate::textures::TextureStore::new(&ctx);
    let res = store
        .ensure(
            rec,
            1.0,
            crate::textures::Use::default(),
            &mut Default::default(),
        )
        .unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let transfer = Transfer::new(&ctx, &cache, GobOrder::Tegra).unwrap();
    let output = Arc::new(DataBuffer::new(&ctx, 10 * 16).unwrap());
    let lib = ShaderLibrary::from_source(&ctx, "sample_mips", r#"
        #include <metal_stdlib>
        using namespace metal;
        kernel void sample_mips(device float4* dst [[buffer(0)]], texture2d_array<float> tex [[texture(0)]],
            uint2 gid [[thread_position_in_grid]]) {
            constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::nearest, mip_filter::nearest);
            dst[gid.y * 5 + gid.x] = tex.sample(s, float2(0.5), gid.y, level(float(gid.x)));
        }
    "#).unwrap();
    let pipeline = cache.compute(123, &lib, "sample_mips").unwrap();
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    transfer
        .upload(frame.commands(), &scratch, &pools, rec, &res, false)
        .unwrap();
    {
        let mut cp = frame.commands().begin_compute_pass(&table).unwrap();
        cp.encode(&ComputeCommand::Depend(Dependency::between(
            Use::CopyWrite,
            Use::ComputeRead,
        )))
        .unwrap();
        cp.encode(&ComputeCommand::SetPipeline(pipeline)).unwrap();
        cp.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: output.gpu_address(),
        })
        .unwrap();
        cp.encode(&ComputeCommand::SetTexture {
            index: 0,
            texture: res.tex.clone(),
        })
        .unwrap();
        cp.encode(&ComputeCommand::DispatchThreads {
            grid: [5, 2, 1],
            threads: [5, 2, 1],
        })
        .unwrap();
        cp.retain(output.clone());
        cp.end();
    }
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let mut bytes = [0; 160];
    output.read(0, &mut bytes).unwrap();
    for slice in 0..2 {
        for level in 0..5 {
            let at = (slice * 5 + level) * 16;
            let rgba: Vec<f32> = bytes[at..at + 16]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            let expected = if slice == 0 {
                (31 - level) as f32 / 31.0
            } else {
                (63 - level) as f32 / 63.0
            };
            assert!(
                (rgba[slice] - expected).abs() < 0.005,
                "slice {slice} mip {level}: {rgba:?}, expected {expected}"
            );
            assert_eq!(rgba[1 - slice], 0.0);
            assert_eq!(rgba[3], 1.0);
        }
    }
}

#[test]
fn textures_the_kernel_cannot_write_go_through_a_buffer() {
    use nxkit_metal::BcKind;
    assert!(buffered::via_buffer(Format::Depth16Unorm));
    assert!(buffered::via_buffer(Format::Depth32Float));
    assert!(buffered::via_buffer(Format::Stencil8));
    assert!(buffered::via_buffer(Format::Bc(BcKind::Bc4Unorm)));
    assert!(!buffered::via_buffer(Format::Depth32FloatStencil8));
    assert!(!buffered::via_buffer(Format::R16Uint));
    assert!(!buffered::via_buffer(Format::Rgba8Unorm));
}

#[test]
fn buffered_levels_round_each_level_to_whole_blocks_and_keep_the_depth() {
    // A 64x64x64 BC4 3D texture is 16x16 blocks deep 64; a 2D chain keeps 1x1 blocks for its tail levels.
    assert_eq!(
        buffered::level_blocks(&[[64, 64, 64]], (4, 4)),
        vec![[16, 16, 64]]
    );
    assert_eq!(
        buffered::level_blocks(&[[10, 6, 1], [5, 3, 1], [2, 1, 1], [1, 1, 0]], (4, 4)),
        vec![[3, 2, 1], [2, 1, 1], [1, 1, 1], [1, 1, 1]]
    );
    assert_eq!(
        buffered::level_blocks(&[[4096, 4096, 1]], (1, 1)),
        vec![[4096, 4096, 1]]
    );
}

/// A backing with a real address of its own, so a pool is told apart from the next one.
struct Fake(u64);

impl Backing for Fake {
    fn real_base(&self) -> u64 {
        self.0
    }
}

struct NoPins;

impl crate::pools::PagePins for NoPins {
    fn pin(&self, _start: u64, _end: u64) {}

    fn unpin(&self, _start: u64, _end: u64) {}
}

/// Two pools of 0x4000 bytes at 0x4000 and 0x10_0000, each with a buffer at a real address of its own.
fn two_pools() -> crate::pools::PoolSet<Fake> {
    let set = crate::pools::PoolSet::new(
        0,
        0,
        |host: u64, _len: u64| Ok(Fake(host + 0x1_0000_0000)),
        Arc::new(NoPins),
    );
    set.register(0x4000, 0x4000, 0).unwrap();
    set.register(0x10_0000, 0x4000, 0).unwrap();
    set
}

#[test]
fn locate_gives_the_buffer_of_the_pool_and_the_offset_in_it() {
    let snap = two_pools().current();
    let (mem, offset) = locate(&snap, 0x10_1000, 0x2000).unwrap();
    assert_eq!(mem.0, 0x1_0010_0000);
    assert_eq!(offset, 0x1000);
    let (mem, offset) = locate(&snap, 0x4000, 0x4000).unwrap();
    assert_eq!(mem.0, 0x1_0000_4000);
    assert_eq!(offset, 0);
}

#[test]
fn locate_skips_a_range_that_is_not_whole_in_one_pool() {
    let snap = two_pools().current();
    // across the end of the pool
    assert_eq!(locate(&snap, 0x7000, 0x2000).err(), Some(Skip::Buffer));
    // in the gap between the pools, and below the first one
    assert_eq!(locate(&snap, 0x8000, 1).err(), Some(Skip::Buffer));
    assert_eq!(locate(&snap, 0x100, 1).err(), Some(Skip::Buffer));
    // one byte past the last byte of the last pool
    assert_eq!(locate(&snap, 0x10_4000, 1).err(), Some(Skip::Buffer));
}

#[test]
fn a_texture_across_the_end_of_its_pool_is_not_uploaded() {
    let Ok(ctx) = Context::system_default() else {
        eprintln!("skipped: no usable Metal 4 device");
        return;
    };
    let guest = Arc::new(GuestMemory::allocate(&ctx, 1024 * 1024).unwrap());
    let base = nxkit_metal::GUEST_VA_BASE;
    // The pool ends 0x1000 bytes into the texture, which needs more.
    let pools = GuestPools::whole(base, 0x1000 + 0x1000, guest.clone());
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::D2,
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
    let mut resources = ResourceTable::new();
    let id = resources
        .create(desc, GpuAddr::new(base + 0x1000).unwrap())
        .unwrap();
    let rec = *resources.get(id).unwrap();
    assert!(rec.layout.size > 0x1000);
    let mut store = crate::textures::TextureStore::new(&ctx);
    let res = store
        .ensure(
            &rec,
            1.0,
            crate::textures::Use::default(),
            &mut Default::default(),
        )
        .unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let transfer = Transfer::new(&ctx, &cache, GobOrder::Tegra).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let r = transfer.upload(frame.commands(), &scratch, &pools, &rec, &res, false);
    assert_eq!(r, Err(Skip::Buffer));
    let items = [(&rec, &res, &Dirty::default())];
    let back = transfer.copy_back_many(frame.commands(), &scratch, &pools, &items);
    assert_eq!(back, vec![Err(Skip::Buffer)]);
}
