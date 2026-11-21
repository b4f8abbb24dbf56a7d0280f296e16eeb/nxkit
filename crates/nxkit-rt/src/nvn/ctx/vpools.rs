//! The virtual memory pools of the context: the model of `nxkit_nvn::vspace` behind a lock of its own, and the changes
//! that the front end has not heard of yet.
//!
//! The lock of the model is a leaf: nothing takes the front lock while it holds it, so a memory pool call never waits
//! for the encode of a frame. A mapping request or a CPU write to a physical page behind a virtual one queues the
//! virtual range, and [`Gpu::lock`] hands it to the front end ([`Front::virtual_changed`]) before anything decodes,
//! as it does with the flushed ranges.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_nvn::error::{NvnError, Result as NvnResult};
use nxkit_nvn::space::{GpuAddr, GpuSpace};
use nxkit_nvn::vspace::{PageTables, VirtualSpace};

use super::{Front, Gpu};

/// How a virtual pool reaches its memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VMode {
    /// It has a range of its own and no mapping yet: the first mapping request decides.
    Own,
    /// A window on one physical pool with the same offsets: the storage of the pool is the physical pool's memory
    /// (the range of its own went back).
    Alias,
    /// Its own range, read through the page table.
    Sparse,
}

/// A virtual pool of the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VPool {
    /// The first GPU address of its own range, 0 for an alias.
    pub(crate) base: u64,
    /// Its size in bytes.
    pub(crate) size: u64,
    pub(crate) mode: VMode,
    /// An address of its own range left the host (a texture was made in it, the game asked for its address): an alias
    /// would move it, so the first mapping makes the pool sparse.
    pub(crate) taken: bool,
}

#[derive(Debug, Default)]
struct Inner {
    /// None when the backend has no window for virtual pools.
    space: Option<VirtualSpace>,
    /// The virtual pools with a range of their own or an alias, by the guest address of their `NVNmemoryPool`.
    pools: HashMap<u64, VPool>,
}

/// The model and the queued changes.
#[derive(Debug, Default)]
pub(crate) struct VPools {
    inner: Mutex<Inner>,
    /// The virtual ranges that changed since the last [`Gpu::lock`].
    pending: Mutex<Vec<(GpuAddr, u64)>>,
}

impl VPools {
    /// The pools over the window `(start, len)` of the backend. A window that the model refuses is logged and gives
    /// no virtual pools.
    pub(crate) fn new(window: Option<(u64, u64)>) -> Self {
        let space = window.and_then(|(start, len)| match VirtualSpace::new(start, len) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!(
                    "nxkit-rt: warning: the window of the virtual memory pools {start:#x}+{len:#x} is refused: {e}"
                );
                None
            }
        });
        Self {
            inner: Mutex::new(Inner {
                space,
                pools: HashMap::new(),
            }),
            pending: Mutex::new(Vec::new()),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, Vec<(GpuAddr, u64)>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Gpu {
    /// `nvnMemoryPoolInitialize` of a virtual pool at `pool` without storage: it gets a range of `size` bytes of GPU
    /// addresses of its own. None when the backend has no window or the window is full.
    pub(crate) fn vpool_init(&self, pool: u64, size: u64) -> Option<u64> {
        let mut g = self.vpools.inner();
        let base = match g.space.as_mut()?.reserve(size) {
            Ok(a) => a.get(),
            Err(e) => {
                eprintln!(
                    "nxkit-rt: warning: a virtual memory pool of {size:#x} bytes gets no range: {e}"
                );
                return None;
            }
        };
        let old = g.pools.insert(
            pool,
            VPool {
                base,
                size,
                mode: VMode::Own,
                taken: false,
            },
        );
        if let Some(old) = old {
            release(&mut g, old);
        }
        Some(base)
    }

    /// The virtual pool at `pool`, None when it has no range of its own and no alias.
    pub(crate) fn vpool(&self, pool: u64) -> Option<VPool> {
        self.vpools.inner().pools.get(&pool).copied()
    }

    /// An address of the range of `pool` leaves the host. The result is the first address of the range, None when
    /// the pool has none (it is no virtual pool or an alias).
    pub(crate) fn vpool_take(&self, pool: u64) -> Option<u64> {
        let mut g = self.vpools.inner();
        let v = g.pools.get_mut(&pool)?;
        if v.mode == VMode::Alias {
            return None;
        }
        v.taken = true;
        Some(v.base)
    }

    /// The first mapping request of a pool in [`VMode::Own`] decides: an alias when `alias` (the request keeps the
    /// offsets of a physical pool) and no address of the range left the host, sparse otherwise. The result is the mode
    /// after the call.
    pub(crate) fn vpool_decide(&self, pool: u64, alias: bool) -> Option<VMode> {
        let mut g = self.vpools.inner();
        let v = *g.pools.get(&pool)?;
        if v.mode != VMode::Own {
            return Some(v.mode);
        }
        let mode = if alias && !v.taken {
            if let (Some(s), Ok(b)) = (g.space.as_mut(), GpuAddr::new(v.base)) {
                s.release(b);
            }
            VMode::Alias
        } else {
            VMode::Sparse
        };
        let e = g.pools.get_mut(&pool)?;
        e.mode = mode;
        if mode == VMode::Alias {
            e.base = 0;
        }
        Some(mode)
    }

    /// `nvnMemoryPoolFinalize` of a virtual pool: its range is free again.
    pub(crate) fn vpool_finalize(&self, pool: u64) {
        let mut g = self.vpools.inner();
        if let Some(v) = g.pools.remove(&pool) {
            release(&mut g, v);
        }
    }

    /// `nvnMemoryPoolMapVirtual` of a sparse pool: maps `[voff, voff + size)` of the pool at the GPU address `base`
    /// onto the guest memory at the host address `phys`, or unmaps it when `phys` is None. The textures in the range
    /// load again.
    pub(crate) fn vpool_map(
        &self,
        base: u64,
        voff: u64,
        size: u64,
        phys: Option<u64>,
    ) -> NvnResult<()> {
        let base = GpuAddr::new(base)?;
        let phys = phys.map(|h| self.space.gpu_address(h, size)).transpose()?;
        let changed = {
            let mut g = self.vpools.inner();
            let s = g.space.as_mut().ok_or(NvnError::Unsupported(
                "the backend has no window for virtual memory pools",
            ))?;
            s.map(base, voff, size, phys)?
        };
        self.vpools.queue().push(changed);
        self.writes_dirty.store(true, Ordering::Release);
        Ok(())
    }

    /// The CPU wrote `[host, host + len)` of guest memory: the virtual pages that show those bytes changed too.
    pub(super) fn vpool_wrote(&self, host: u64, len: u64) {
        let Ok(gpu) = self.space.gpu_address(host, len) else {
            return;
        };
        let mut hit = Vec::new();
        if let Some(s) = self.vpools.inner().space.as_ref() {
            s.aliases(gpu.get(), len, &mut hit);
        }
        if !hit.is_empty() {
            self.vpools.queue().extend(hit);
            self.writes_dirty.store(true, Ordering::Release);
        }
    }

    /// Hands the queued virtual ranges to the front end. [`Gpu::lock`] calls it.
    pub(super) fn vpool_apply(&self, front: &mut Front) {
        let ranges = std::mem::take(&mut *self.vpools.queue());
        for (start, len) in ranges {
            front.virtual_changed(start, len);
        }
    }

    /// The page tables for a flush, None when the game has no virtual pool.
    pub(super) fn vpool_snapshot(&self) -> Option<Arc<PageTables>> {
        self.vpools
            .inner()
            .space
            .as_ref()
            .map(VirtualSpace::snapshot)
            .filter(|t| !t.is_empty())
    }
}

/// Gives the range of `v` back, with its pages.
fn release(g: &mut Inner, v: VPool) {
    if let (Some(s), Ok(b), true) = (g.space.as_mut(), GpuAddr::new(v.base), v.base != 0) {
        s.release(b);
    }
}
