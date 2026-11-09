//! No-copy buffers over memory that other no-copy buffers cover too: the facts that one buffer per memory pool rests on
//! (`docs/plans/gpu-buffer-per-pool.md`, B.3 and B.4). Without a Metal device the tests print a message and pass.
//!
//! `GuestMemory::from_raw` commits the residency set when it makes a buffer and when it drops one. So the creation
//! times below include one commit each.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;
use std::time::Instant;

use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ComputeCommand, ComputePipeline, Context, DataBuffer, FrameRing,
    GuestMemory, PAGE_SIZE, PipelineCache, ShaderLibrary,
};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

fn ctx() -> Option<Context> {
    match Context::system_default() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            None
        }
    }
}

fn pattern(n: usize, seed: u32) -> Vec<u8> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as u8
        })
        .collect()
}

/// Page aligned memory that the test owns. The buffers over it must be dropped first, so declare it before them.
struct Region {
    base: NonNull<u8>,
    layout: Layout,
}

impl Region {
    fn new(len: u64) -> Self {
        let layout = Layout::from_size_align(len as usize, PAGE_SIZE as usize).unwrap();
        // SAFETY: the layout has a non-zero size.
        let raw = unsafe { alloc_zeroed(layout) };
        Self {
            base: NonNull::new(raw).unwrap(),
            layout,
        }
    }

    /// A no-copy buffer over `[off, off + len)`.
    fn wrap(&self, ctx: &Context, off: u64, len: u64) -> GuestMemory {
        assert!(off + len <= self.layout.size() as u64);
        // SAFETY: the range is inside the allocation, which is page aligned and freed after the buffers (they are
        // declared after the region). `off` and `len` are multiples of the page size in every caller.
        let at = unsafe { NonNull::new_unchecked(self.base.as_ptr().add(off as usize)) };
        // SAFETY: see above. The memory is not moved.
        unsafe { GuestMemory::from_raw(ctx, at, len) }.unwrap()
    }

    /// The CPU writes through the plain pointer, not through a buffer.
    fn cpu_write(&self, off: u64, data: &[u8]) {
        assert!(off as usize + data.len() <= self.layout.size());
        // SAFETY: the range is inside the allocation. `data` is a separate allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.base.as_ptr().add(off as usize),
                data.len(),
            )
        };
    }

    fn cpu_read(&self, off: u64, len: usize) -> Vec<u8> {
        assert!(off as usize + len <= self.layout.size());
        let mut out = vec![0; len];
        // SAFETY: the range is inside the allocation. `out` is a separate allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.base.as_ptr().add(off as usize),
                out.as_mut_ptr(),
                len,
            )
        };
        out
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout and freed once.
        unsafe { dealloc(self.base.as_ptr(), self.layout) };
    }
}

const COPY_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct P { ulong src; ulong dst; uint count; uint pad; };
kernel void copy_words(constant P& p [[buffer(0)]], uint i [[thread_position_in_grid]]) {
    if (i >= p.count) { return; }
    device const uint* src = (device const uint*)p.src;
    device uint* dst = (device uint*)p.dst;
    dst[i] = src[i];
}
"#;

/// A compute kernel that copies words from one GPU address to another, one command buffer per copy.
struct Rig {
    ctx: Context,
    pipe: ComputePipeline,
    table: ArgumentTable,
    ring: FrameRing,
}

impl Rig {
    fn new(ctx: &Context) -> Self {
        let cache = PipelineCache::new(ctx, &ArchiveConfig::default()).unwrap();
        let lib = ShaderLibrary::from_source(ctx, "copy", COPY_MSL).unwrap();
        Self {
            ctx: ctx.clone(),
            pipe: cache.compute(20, &lib, "copy_words").unwrap(),
            table: ArgumentTable::new(ctx, 1, 0).unwrap(),
            ring: FrameRing::new(ctx).unwrap(),
        }
    }

    /// Copies `len` bytes (a multiple of 4) from GPU address `src` to `dst` and waits for the command buffer.
    fn copy(&mut self, src: u64, dst: u64, len: u64) {
        let mut args = Vec::new();
        args.extend(src.to_le_bytes());
        args.extend(dst.to_le_bytes());
        args.extend(((len / 4) as u32).to_le_bytes());
        args.extend(0u32.to_le_bytes());
        let mut frame = self.ring.begin().unwrap();
        let p = frame.scratch().push(&args).unwrap();
        {
            let mut pass = frame.commands().begin_compute_pass(&self.table).unwrap();
            pass.encode(&ComputeCommand::SetPipeline(self.pipe.clone()))
                .unwrap();
            pass.encode(&ComputeCommand::SetBuffer {
                index: 0,
                gpu_addr: p,
            })
            .unwrap();
            pass.encode(&ComputeCommand::DispatchThreads {
                grid: [(len / 4) as u32, 1, 1],
                threads: [16, 1, 1],
            })
            .unwrap();
            pass.end();
        }
        self.ctx.commit_residency();
        frame.submit().unwrap().wait().unwrap();
    }

    /// Reads `len` bytes at a GPU address into a fresh `DataBuffer` and returns them.
    fn read(&mut self, src: u64, len: u64) -> Vec<u8> {
        let out = DataBuffer::new(&self.ctx, len).unwrap();
        self.copy(src, out.gpu_address(), len);
        let mut bytes = vec![0; len as usize];
        out.read(0, &mut bytes).unwrap();
        bytes
    }

    /// Writes `data` to a GPU address from a fresh `DataBuffer`.
    fn write(&mut self, dst: u64, data: &[u8]) {
        let src = DataBuffer::new(&self.ctx, data.len() as u64).unwrap();
        src.write(0, data).unwrap();
        self.copy(src.gpu_address(), dst, data.len() as u64);
    }
}

/// Cases 1 to 4 of B.3: two buffers over one 16 KiB page (`A = [0, 32K)`, `B = [16K, 48K)`).
#[test]
fn overlapping_buffers_see_the_same_bytes() {
    let Some(ctx) = ctx() else { return };
    let mut rig = Rig::new(&ctx);
    let region = Region::new(64 * KIB);
    let a = region.wrap(&ctx, 0, 32 * KIB);
    let b = region.wrap(&ctx, 16 * KIB, 32 * KIB);

    // (4) both buffers start on a page boundary.
    let (a_gpu, b_gpu) = (
        a.gpu_address_of_offset(0, 1).unwrap(),
        b.gpu_address_of_offset(0, 1).unwrap(),
    );
    assert_eq!(a_gpu % PAGE_SIZE, 0, "A gpuAddress {a_gpu:#x}");
    assert_eq!(b_gpu % PAGE_SIZE, 0, "B gpuAddress {b_gpu:#x}");
    eprintln!("A at {a_gpu:#x}, B at {b_gpu:#x}, they overlap by 16 KiB");

    // (1) the CPU writes at 20K; the GPU reads it through A at 20K and through B at 4K.
    let first = pattern(64, 1);
    region.cpu_write(20 * KIB, &first);
    assert_eq!(rig.read(a_gpu + 20 * KIB, 64), first, "read through A");
    assert_eq!(rig.read(b_gpu + 4 * KIB, 64), first, "read through B");

    // (2) the GPU writes through A at 24K; the CPU sees it, and a second command buffer reads it through B at 8K.
    let second = pattern(64, 2);
    rig.write(a_gpu + 24 * KIB, &second);
    assert_eq!(region.cpu_read(24 * KIB, 64), second, "CPU after the write");
    let mut via_a = [0; 64];
    a.read(24 * KIB, &mut via_a).unwrap();
    assert_eq!(via_a.as_slice(), second.as_slice());
    assert_eq!(rig.read(b_gpu + 8 * KIB, 64), second, "read through B");

    // (3) drop A; B still reads and writes.
    drop(a);
    assert_eq!(rig.read(b_gpu + 4 * KIB, 64), first, "B after A is gone");
    assert_eq!(rig.read(b_gpu + 8 * KIB, 64), second, "B after A is gone");
    let third = pattern(64, 3);
    rig.write(b_gpu + 12 * KIB, &third);
    assert_eq!(region.cpu_read(28 * KIB, 64), third, "write through B");
}

/// Case 5: ten thousand buffers of one page each over 160 MiB.
#[test]
fn ten_thousand_page_buffers_over_one_region() {
    const COUNT: u64 = 10_000;
    let Some(ctx) = ctx() else { return };
    let region = Region::new(160 * MIB);
    let mut buffers = Vec::with_capacity(COUNT as usize);
    let mut slowest = 0.0f64;
    let t0 = Instant::now();
    for i in 0..COUNT {
        let t = Instant::now();
        buffers.push(region.wrap(&ctx, i * PAGE_SIZE, PAGE_SIZE));
        slowest = slowest.max(t.elapsed().as_secs_f64());
    }
    let made = t0.elapsed();
    eprintln!(
        "{COUNT} buffers of 16 KiB: {:.1} ms in all, {:.1} us per buffer (each with its residency commit), slowest {:.2} ms",
        made.as_secs_f64() * 1e3,
        made.as_secs_f64() * 1e6 / COUNT as f64,
        slowest * 1e3
    );
    let t = Instant::now();
    ctx.commit_residency();
    eprintln!(
        "one more residency commit with nothing pending: {:.3} ms, {} allocations resident",
        t.elapsed().as_secs_f64() * 1e3,
        ctx.resident_count()
    );
    for (i, g) in buffers.iter().enumerate() {
        let gpu = g.gpu_address_of_offset(0, PAGE_SIZE).unwrap();
        assert_ne!(gpu, 0, "buffer {i} has no address");
    }
    let t = Instant::now();
    buffers.clear();
    eprintln!(
        "dropping {COUNT} buffers (each with its commit): {:.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
}

/// B.4: a buffer over a range is dropped and a new one is made over the same range. The bytes that the CPU wrote in
/// between are what the GPU reads through the new buffer, and what it writes goes to the same memory.
#[test]
fn a_new_buffer_over_the_same_range_sees_the_bytes_written_in_between() {
    let Some(ctx) = ctx() else { return };
    let mut rig = Rig::new(&ctx);
    let region = Region::new(64 * KIB);
    let old = region.wrap(&ctx, 16 * KIB, 32 * KIB);
    let before = pattern(64, 4);
    rig.write(old.gpu_address_of_offset(8 * KIB, 64).unwrap(), &before);
    assert_eq!(region.cpu_read(24 * KIB, 64), before);
    drop(old);

    let between = pattern(64, 5);
    region.cpu_write(24 * KIB, &between);
    region.cpu_write(40 * KIB, &between);

    let new = region.wrap(&ctx, 16 * KIB, 32 * KIB);
    let gpu = new.gpu_address_of_offset(0, 1).unwrap();
    assert_eq!(gpu % PAGE_SIZE, 0);
    assert_eq!(rig.read(gpu + 8 * KIB, 64), between, "rewritten range");
    assert_eq!(rig.read(gpu + 24 * KIB, 64), between, "new range");
    let after = pattern(64, 6);
    rig.write(gpu + 12 * KIB, &after);
    assert_eq!(region.cpu_read(28 * KIB, 64), after, "write through new");
}

/// B.4: buffers from `from_raw_uncommitted` join the residency set without a commit, work after one commit for the
/// batch, and leave the set when they drop. The count shows an add at once, but a removal only after the commit.
#[test]
fn uncommitted_buffers_work_after_one_commit() {
    let Some(ctx) = ctx() else { return };
    let mut rig = Rig::new(&ctx);
    let region = Region::new(64 * KIB);
    let before = ctx.resident_count();

    let make = |off: u64, len: u64| {
        assert!(off + len <= region.layout.size() as u64);
        // SAFETY: the range is inside the page aligned allocation, which outlives the buffers (they are declared
        // after the region), and `off` and `len` are multiples of the page size.
        let at = unsafe { NonNull::new_unchecked(region.base.as_ptr().add(off as usize)) };
        // SAFETY: see above. The memory is not moved.
        unsafe { GuestMemory::from_raw_uncommitted(&ctx, at, len) }.unwrap()
    };
    let a = make(0, 32 * KIB);
    let b = make(16 * KIB, 32 * KIB);
    assert_eq!(ctx.resident_count(), before + 2, "both are in the set");
    ctx.commit_residency();

    let (a_gpu, b_gpu) = (
        a.gpu_address_of_offset(0, 1).unwrap(),
        b.gpu_address_of_offset(0, 1).unwrap(),
    );
    let data = pattern(64, 7);
    region.cpu_write(20 * KIB, &data);
    assert_eq!(rig.read(a_gpu + 20 * KIB, 64), data, "read through A");
    assert_eq!(rig.read(b_gpu + 4 * KIB, 64), data, "read through B");

    // The rig's reads add allocations of their own (scratch chunks), so count again right before the drops.
    // The rig drops its own buffers without a commit, and a removal shows only after a commit. Commit first, so the
    // base count has no pending removal.
    ctx.commit_residency();
    let resident = ctx.resident_count();
    drop(a);
    drop(b);
    // Measured: an add shows in the count at once, but a removal shows only after the commit.
    ctx.commit_residency();
    assert_eq!(
        ctx.resident_count(),
        resident - 2,
        "both left the set (the count follows a removal only after the commit)"
    );
}
