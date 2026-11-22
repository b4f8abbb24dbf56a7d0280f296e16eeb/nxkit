//! Guest memory: one no-copy `MTLBuffer` over host memory that the caller owns.
//!
//! The GPU address that the game sees is `buffer.gpuAddress + (host_addr - host_base)`. A shader reads guest memory
//! through a `device` pointer made from that address, so the buffer is in the residency set from the start.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLBuffer, MTLDevice, MTLResourceOptions};

use crate::address::{AddressMap, check_outside_window};
use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::rawcopy::{copy_in, copy_out};

/// The page size of Apple Silicon. A no-copy buffer needs a base and a length that are multiples of it.
pub const PAGE_SIZE: u64 = 16 * 1024;

/// Memory that `allocate` or `map_below` made. It is freed on drop.
enum OwnedRegion {
    /// From the allocator, with its layout.
    Heap(NonNull<u8>, Layout),
    /// An anonymous mapping of this many bytes.
    Mapped(NonNull<u8>, usize),
}

impl Drop for OwnedRegion {
    fn drop(&mut self) {
        match *self {
            // SAFETY: the pointer and the layout are those of the allocation in `GuestMemory::allocate`. It is freed
            // once.
            Self::Heap(p, layout) => unsafe { dealloc(p.as_ptr(), layout) },
            // SAFETY: the pointer and the length are those of the mapping in `GuestMemory::map_below`. It is unmapped
            // once.
            Self::Mapped(p, len) => unsafe {
                munmap(p.as_ptr().cast(), len);
            },
        }
    }
}

const PROT_RW: i32 = 3;
const MAP_PRIVATE: i32 = 0x2;
const MAP_ANON: i32 = 0x1000;
/// What `mmap` gives on failure.
const MAP_FAILED: usize = usize::MAX;

unsafe extern "C" {
    fn mmap(
        addr: *mut std::ffi::c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        off: i64,
    ) -> *mut std::ffi::c_void;
    fn munmap(addr: *mut std::ffi::c_void, len: usize) -> i32;
}

/// Maps `len` zeroed bytes, read and write, with `hint` as the address hint (0 for none). The system may place the
/// mapping elsewhere. None when the system has no memory.
fn map_anon(hint: u64, len: usize) -> Option<NonNull<u8>> {
    // SAFETY: an anonymous private mapping without `MAP_FIXED` replaces nothing. The hint is only a hint.
    let p = unsafe {
        mmap(
            hint as *mut std::ffi::c_void,
            len,
            PROT_RW,
            MAP_PRIVATE | MAP_ANON,
            -1,
            0,
        )
    };
    if p as usize == MAP_FAILED {
        return None;
    }
    NonNull::new(p.cast())
}

/// The guest memory of the host as seen by the GPU.
///
/// A buffer joins the residency set when it is made, but the GPU may use it only after the next
/// [`Context::commit_residency`]. [`GuestMemory::allocate`] and [`GuestMemory::from_raw`] commit when they make a
/// buffer and when they drop one. A buffer from [`GuestMemory::from_raw_uncommitted`] commits neither: whoever makes
/// or drops such buffers must call `commit_residency` once per batch, before the GPU work that uses a new buffer is
/// submitted, and after a drop to let the set release the pages.
pub struct GuestMemory {
    buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    map: AddressMap,
    base: NonNull<u8>,
    ctx: Context,
    /// True when the buffer commits the residency set on drop. Buffers made by `from_raw_uncommitted` leave that to
    /// the caller.
    commits: bool,
    _tracked: crate::memstats::Tracked,
    // Last field: it drops after `buffer`, so Metal never sees a dangling range.
    #[allow(dead_code)] // owns the memory behind the buffer
    owned: Option<OwnedRegion>,
}

// SAFETY: the pointers refer to memory that is valid for the life of the struct. The buffer object is thread safe.
// Concurrent reads and writes of the same bytes are the caller's to order, as for any shared memory.
unsafe impl Send for GuestMemory {}
// SAFETY: see `Send`. All methods take `&self` and only copy bytes in and out of the region.
unsafe impl Sync for GuestMemory {}

impl GuestMemory {
    /// Allocates `len` zeroed bytes, page aligned, and wraps them. `len` is rounded up to whole pages.
    pub fn allocate(ctx: &Context, len: u64) -> Result<Self> {
        let len = len.div_ceil(PAGE_SIZE) * PAGE_SIZE;
        let size =
            usize::try_from(len).map_err(|_| MetalError::Invalid("length does not fit usize"))?;
        let layout = Layout::from_size_align(size, PAGE_SIZE as usize)
            .map_err(|_| MetalError::Invalid("bad layout"))?;
        if size == 0 {
            return Err(MetalError::Invalid("empty guest memory"));
        }
        // SAFETY: the layout has a non-zero size.
        let raw = unsafe { alloc_zeroed(layout) };
        let base = NonNull::new(raw).ok_or_else(|| create("guest memory", "out of memory"))?;
        let owned = OwnedRegion::Heap(base, layout);
        // SAFETY: `base` is valid for `len` bytes until `owned` drops, which happens after the buffer is released.
        unsafe { Self::wrap(ctx, base, len, Some(owned), true) }
    }

    /// Maps `len` zeroed bytes (rounded up to whole pages) that end at or below `limit`, and wraps them. The memory is an
    /// anonymous mapping of its own, outside every other mapping of the process, and it is unmapped on drop. The hints
    /// are tried in order (0 lets the system choose). When a mapping ends above `limit`, the call gives it back and
    /// tries the next hint. A caller whose GPU addresses must fit an address space of its own (`1 << 39` for the
    /// Switch) uses it.
    pub fn map_below(ctx: &Context, len: u64, limit: u64, hints: &[u64]) -> Result<Self> {
        let len = len.max(1).div_ceil(PAGE_SIZE) * PAGE_SIZE;
        let size =
            usize::try_from(len).map_err(|_| MetalError::Invalid("length does not fit usize"))?;
        for &hint in hints {
            let Some(base) = map_anon(hint, size) else {
                return Err(create(
                    "guest memory",
                    format!("mmap of {len} bytes failed"),
                ));
            };
            let end = (base.as_ptr() as u64).saturating_add(len);
            if end > limit {
                // SAFETY: the mapping was made just above with this length and nothing else knows it.
                unsafe { munmap(base.as_ptr().cast(), size) };
                continue;
            }
            let owned = OwnedRegion::Mapped(base, size);
            // SAFETY: `base` is valid for `len` bytes until `owned` drops, which happens after the buffer is released.
            return unsafe { Self::wrap(ctx, base, len, Some(owned), true) };
        }
        Err(create(
            "guest memory",
            format!("no mapping of {len} bytes ends at or below {limit:#x} (hints {hints:#x?})"),
        ))
    }

    /// Wraps memory that the caller owns, for example the mapping of the guest address space.
    ///
    /// # Safety
    ///
    /// `base` must be valid for reads and writes of `len` bytes for as long as the returned value lives.
    /// The memory must not be unmapped or moved. `base` and `len` must be multiples of [`PAGE_SIZE`].
    pub unsafe fn from_raw(ctx: &Context, base: NonNull<u8>, len: u64) -> Result<Self> {
        // SAFETY: the contract of this function is the contract of `wrap`.
        unsafe { Self::wrap(ctx, base, len, None, true) }
    }

    /// Like [`GuestMemory::from_raw`], but the residency set is not committed, neither here nor when the value drops.
    /// The buffer is in the set, and the GPU may use it only after the caller's next [`Context::commit_residency`].
    /// Many buffers made together need one commit. After the last drop of a batch, commit too, so the pages leave the
    /// set.
    ///
    /// # Safety
    ///
    /// The same as for [`GuestMemory::from_raw`]: `base` must be valid for reads and writes of `len` bytes for as long
    /// as the returned value lives, the memory must not be unmapped or moved, and `base` and `len` must be multiples
    /// of [`PAGE_SIZE`].
    pub unsafe fn from_raw_uncommitted(ctx: &Context, base: NonNull<u8>, len: u64) -> Result<Self> {
        // SAFETY: the contract of this function is the contract of `wrap`.
        unsafe { Self::wrap(ctx, base, len, None, false) }
    }

    /// # Safety
    ///
    /// `base` is valid for `len` bytes for the life of the result. `commit` says whether this call and the drop of the
    /// result commit the residency set.
    unsafe fn wrap(
        ctx: &Context,
        base: NonNull<u8>,
        len: u64,
        owned: Option<OwnedRegion>,
        commit: bool,
    ) -> Result<Self> {
        let addr = base.as_ptr() as u64;
        for v in [addr, len] {
            if v % PAGE_SIZE != 0 {
                return Err(MetalError::Unaligned {
                    value: v,
                    align: PAGE_SIZE,
                });
            }
        }
        let max = ctx.max_buffer_length();
        if len > max {
            return Err(create(
                "guest buffer",
                format!("{len} bytes is above the device limit {max}"),
            ));
        }
        // SAFETY: `base` is valid for `len` bytes (the contract of this function). No deallocator is given, because
        // `GuestMemory` keeps the memory alive for as long as the buffer exists.
        let buffer = unsafe {
            ctx.device()
                .newBufferWithBytesNoCopy_length_options_deallocator(
                    base.cast(),
                    len as usize,
                    MTLResourceOptions::StorageModeShared,
                    None,
                )
        }
        .ok_or_else(|| create("guest buffer", "newBufferWithBytesNoCopy returned nil"))?;
        check_outside_window("guest buffer", buffer.gpuAddress(), len)?;
        let map = AddressMap::new(addr, buffer.gpuAddress(), len)?;
        ctx.add_allocation(ProtocolObject::from_ref(&*buffer));
        if commit {
            ctx.commit_residency();
        }
        Ok(Self {
            buffer,
            map,
            base,
            ctx: ctx.clone(),
            commits: commit,
            _tracked: crate::memstats::Tracked::new(crate::memstats::Kind::Guest, len),
            owned,
        })
    }

    /// The map between host addresses and the REAL GPU addresses of the buffer (`gpu_base` is
    /// [`GuestMemory::real_base`]). The guest GPU addresses of the game are virtual and come from
    /// [`crate::GuestSpace::address_map`].
    pub fn address_map(&self) -> AddressMap {
        self.map
    }

    /// The real GPU address of a host address. `len` is the number of bytes the shader will touch.
    pub fn gpu_address(&self, host_addr: u64, len: u64) -> Result<u64> {
        self.map.gpu_address(host_addr, len)
    }

    /// The real GPU address of a byte offset into the guest memory.
    pub fn gpu_address_of_offset(&self, offset: u64, len: u64) -> Result<u64> {
        self.map.gpu_address_of_offset(offset, len)
    }

    /// The length in bytes.
    pub fn len(&self) -> u64 {
        self.map.len()
    }

    /// Always false.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The host address of the first byte.
    pub fn host_base(&self) -> u64 {
        self.map.host_base()
    }

    /// The real GPU address of the first byte: the `gpuAddress` of the buffer. Only Metal and the shaders know it.
    /// It is not the guest GPU address of the game, which starts at `GUEST_VA_BASE` (see [`crate::GuestSpace`]).
    pub fn real_base(&self) -> u64 {
        self.map.gpu_base()
    }

    pub(crate) fn buffer(&self) -> &ProtocolObject<dyn MTLBuffer> {
        &self.buffer
    }

    /// The host pointer of the first byte. A [`crate::GuestSpace`] over this memory copies through it.
    pub(crate) fn base_ptr(&self) -> NonNull<u8> {
        self.base
    }

    /// Copies bytes out of the guest memory. The offset is from the start of the region.
    pub fn read(&self, offset: u64, out: &mut [u8]) -> Result<()> {
        // SAFETY: `base` is valid for `len` bytes for the life of the value (the contract of `wrap`).
        unsafe { copy_out(self.base, self.map.len(), offset, out) }
    }

    /// Copies bytes into the guest memory. The offset is from the start of the region.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<()> {
        // SAFETY: `base` is valid for `len` bytes for the life of the value (the contract of `wrap`).
        unsafe { copy_in(self.base, self.map.len(), offset, data) }
    }
}

impl Drop for GuestMemory {
    fn drop(&mut self) {
        self.ctx
            .remove_allocation(ProtocolObject::from_ref(&*self.buffer));
        if self.commits {
            self.ctx.commit_residency();
        }
    }
}
