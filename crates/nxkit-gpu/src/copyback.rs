//! The copy-back of GPU-written textures whose storage the CPU can read.
//!
//! Some games read on the CPU what the GPU rendered into a texture, from the same guest memory: BotW's terrain
//! clipmap height array (Link's foot placement) and its grass density masks. A frame that writes a texture whose
//! storage lies in a memory pool without `NVN_MEMORY_POOL_FLAGS_CPU_NO_ACCESS` therefore copies the written levels and
//! layers back into guest memory at its end, on the GPU timeline (one compute pass at the end of the last command
//! buffer, no CPU wait). The front end marks the texture `InSync` afterwards (`EncodeReport::mirrored`), with the
//! write epoch of the record the frame was encoded from, so a CPU write after the encode still wins.
//!
//! Render targets in `CPU_NO_ACCESS` pools, which are most of them, are not touched. A candidate is kept at the
//! native size (not scaled), since the copy-back needs the texels of the guest layout.
//!
//! Which pool counts. A game makes a render target heap as a virtual pool (`VIRTUAL | CPU_NO_ACCESS`, usually also
//! `GPU_CACHED`) and maps it onto a physical pool (`PHYSICAL | COMPRESSIBLE | GPU_NO_ACCESS | CPU_CACHED`, flags 0x18c
//! in Ocarina of Time) with `nvnMemoryPoolMapVirtual`. Both name the same bytes at the same addresses here, but the
//! game and the GPU reach them only through the virtual pool: a `GPU_NO_ACCESS` physical pool is memory the GPU does
//! not touch itself, and the physical pool's `CPU_CACHED` is the mapping of the one that backs it, whose bytes a CPU
//! read would see in the compressed layout of the render target, not as texels. So the virtual window is registered
//! (`add_window`, from `Backend::map_pool_window`) with the flags of the virtual pool, and for its addresses it is
//! newer than the physical pool and wins. The rules of `candidate`:
//!
//! 1. A texture in a pool (or window) that holds all of it and has `CPU_NO_ACCESS` is never copied back.
//! 2. A texture that straddles windows (no single pool holds it) is rejected when the union of the `CPU_NO_ACCESS`
//!    windows covers it and no CPU-accessible window touches it.
//! 3. A texture in no window, in a pool without `CPU_NO_ACCESS`, is copied back, also when that pool has
//!    `GPU_NO_ACCESS` or `COMPRESSIBLE`: nothing says the game does not read it with the CPU, and a wrong guess
//!    loses data, a copy-back only costs time. `GPU_NO_ACCESS` alone never rejects.
//! 4. A texture in no known pool is not copied back.
//! 5. A texture made with `NVN_TEXTURE_FLAGS_COMPRESSIBLE` whose storage lies in a pool with
//!    `NVN_MEMORY_POOL_FLAGS_COMPRESSIBLE` is not copied back. The console stores such a texture with the compression
//!    of the GPU, and its bytes mean nothing to the CPU without the compression tags, so no game reads them with the
//!    CPU. Unreal Engine 4 games put their render targets in one pool without `CPU_NO_ACCESS` (Octopath Traveler:
//!    flags 0xe2), and their copy-back was about 105 MB and 2.3 ms of GPU time each frame.
//!
//! `NXKIT_COPYBACK=0` turns it off. `NXKIT_COPYBACK_MAX_MB` caps the storage copied back per frame (128 MiB).
//! `NXKIT_DIAG=copyback` names each texture the first time it is copied back, and prints a summary now and then.

use nxkit_nvn::format::{Aspects, format_info};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::resource::{TexId, TextureRecord};

use crate::fastmap::FastSet;
use crate::poolreg::Registry;
use crate::textures::Res;
use crate::upload::Dirty;

/// `NVN_MEMORY_POOL_FLAGS_CPU_NO_ACCESS_BIT`.
pub(crate) const CPU_NO_ACCESS: u32 = 0x1;

/// `NVN_MEMORY_POOL_FLAGS_VIRTUAL_BIT`: a window of a virtual pool, not a pool with storage of its own.
pub(crate) const VIRTUAL: u32 = 0x200;

/// `NVN_MEMORY_POOL_FLAGS_COMPRESSIBLE_BIT`.
pub(crate) const COMPRESSIBLE: u32 = 0x80;

/// Why a GPU-written texture is or is not copied back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// It is a candidate.
    Copied,
    /// `NXKIT_COPYBACK=0`.
    Off,
    /// Its contents die with the frame.
    FrameLocal,
    /// The game presents it.
    Presented,
    /// Depth, stencil or compressed: no copy-back kernel.
    Format,
    /// No known pool holds it.
    NoPool,
    /// A physical or plain pool that holds it whole has `CPU_NO_ACCESS`.
    PoolNoCpu,
    /// A virtual window (or the windows together) that holds it has `CPU_NO_ACCESS`.
    WindowNoCpu,
    /// A compressible texture in compressible pool memory: its bytes are compressed on the console.
    Compressed,
}

impl Why {
    const ALL: [Why; 9] = [
        Why::Copied,
        Why::Off,
        Why::FrameLocal,
        Why::Presented,
        Why::Format,
        Why::NoPool,
        Why::PoolNoCpu,
        Why::WindowNoCpu,
        Why::Compressed,
    ];

    fn name(self) -> &'static str {
        match self {
            Why::Copied => "copied",
            Why::Off => "off",
            Why::FrameLocal => "frame-local",
            Why::Presented => "presented",
            Why::Format => "format",
            Why::NoPool => "no-pool",
            Why::PoolNoCpu => "pool-cpu-no-access",
            Why::WindowNoCpu => "window-cpu-no-access",
            Why::Compressed => "compressed",
        }
    }
}

/// The batches after which a copy-back is surely done on the GPU, even without a wait.
const RECENT_BATCHES: u64 = 8;

/// The most textures that `NXKIT_DIAG=copyback` names.
const LOG_TEXTURES: usize = 300;

/// False when `NXKIT_COPYBACK=0`.
pub(crate) fn enabled() -> bool {
    crate::settings::env().copyback
}

/// The storage bytes that one frame may copy back.
pub(crate) fn budget() -> u64 {
    crate::settings::env().copyback_max_mb << 20
}

/// What the encoder knows for the copy-back: the memory pools with their flags, and the copy-backs in flight.
#[derive(Debug, Default)]
pub(crate) struct CopyBack {
    /// The memory pools with their `NVNmemoryPoolFlags`. Every pool stays until its removal. Pools may overlap.
    pools: Registry<u32>,
    /// Textures the game presented. They are never candidates.
    presented: FastSet<TexId>,
    /// The storage ranges copied back in the last batches: (start, end, batch).
    recent: Vec<(u64, u64, u64)>,
    /// The textures that the diagnostic named.
    seen: FastSet<TexId>,
    /// The last decision for each texture (`NXKIT_DIAG=copyback` only). `candidate` takes `&self`.
    decided: std::sync::Mutex<crate::fastmap::FastMap<TexId, Why>>,
    /// Frames with a copy-back, textures and bytes copied back, for the diagnostic.
    frames: u64,
    textures: u64,
    bytes: u64,
}

impl CopyBack {
    /// A memory pool of the game at GPU address `start`. It is added to the pools it overlaps. None is forgotten.
    pub(crate) fn add_pool(&mut self, start: u64, len: u64, flags: u32) {
        self.pools.add_with(start, len, flags);
    }

    /// The memory pool that holds `addr`: its GPU start and end and its flags. When several pools hold it, the
    /// newest one counts: it describes what the game mapped last.
    pub(crate) fn pool_of(&self, addr: u64) -> Option<(u64, u64, u32)> {
        self.pools.newest_holding(addr, addr.checked_add(1)?)
    }

    /// A virtual pool maps `[start, start + len)` of a physical pool, with the flags of the virtual pool. It is the
    /// newest registration for these addresses. A window already registered for exactly this range is replaced.
    pub(crate) fn add_window(&mut self, start: u64, len: u64, flags: u32) {
        let end = start.saturating_add(len);
        self.pools
            .remove_where(|s, e, f| f & VIRTUAL != 0 && s == start && e == end);
        self.pools.add_with(start, len, flags | VIRTUAL);
    }

    /// Drops every virtual window inside `[start, start + len)`. Physical pools stay.
    pub(crate) fn remove_windows(&mut self, start: u64, len: u64) {
        let end = start.saturating_add(len);
        self.pools
            .remove_where(|s, e, f| f & VIRTUAL != 0 && s >= start && e <= end);
    }

    /// The game finalised the pool at GPU address `start`. The oldest pool with that start goes.
    pub(crate) fn remove_pool(&mut self, start: u64) {
        self.pools.remove(start);
    }

    /// The flags of the newest pool that holds all of `[start, end)`, if one does.
    pub(crate) fn pool_flags(&self, start: u64, end: u64) -> Option<u32> {
        self.pools
            .newest_holding(start, end)
            .map(|(_, _, flags)| flags)
    }

    /// Remembers the textures that the frame presents.
    pub(crate) fn note_presented(&mut self, frame: &Frame) {
        for c in &frame.cmds {
            if let Cmd::Present(id) = c {
                self.presented.insert(*id);
            }
        }
    }

    /// The pool flags of a texture that is copied back after a frame wrote it, or `None` when it is not copied back
    /// (see `why` and the rules in the module comment).
    pub(crate) fn candidate(&self, id: TexId, rec: &TextureRecord) -> Option<u32> {
        let (why, flags) = self.why(id, rec);
        self.note(id, rec, why, flags);
        if why == Why::Copied { flags } else { None }
    }

    /// The decision for a texture, and the flags of the pool it was judged by.
    pub(crate) fn why(&self, id: TexId, rec: &TextureRecord) -> (Why, Option<u32>) {
        if !enabled() {
            return (Why::Off, None);
        }
        if rec.frame_local {
            return (Why::FrameLocal, None);
        }
        if self.presented.contains(&id) {
            return (Why::Presented, None);
        }
        let Ok(info) = format_info(rec.desc.format) else {
            return (Why::Format, None);
        };
        if info.aspects != Aspects::Color || info.block_w > 1 || info.block_h > 1 {
            return (Why::Format, None);
        }
        let (start, end) = (rec.address.get(), rec.end());
        let Some(flags) = self.pool_flags(start, end) else {
            return (Why::NoPool, None);
        };
        if flags & CPU_NO_ACCESS != 0 {
            let why = if flags & VIRTUAL != 0 {
                Why::WindowNoCpu
            } else {
                Why::PoolNoCpu
            };
            return (why, Some(flags));
        }
        // No single pool that is newer holds the texture whole, so the pool that does is the physical one, or an
        // older one. It stays hidden from the CPU when windows without CPU access cover it all.
        let window = |f: u32| f & VIRTUAL != 0;
        if !self
            .pools
            .any_overlap(start, end, |f| window(f) && f & CPU_NO_ACCESS == 0)
            && self
                .pools
                .covered_by(start, end, |f| window(f) && f & CPU_NO_ACCESS != 0)
        {
            return (Why::WindowNoCpu, Some(flags));
        }
        // The physical pool under a window decides the compression, so any pool that holds it whole counts.
        if rec.desc.flags & nxkit_nvn::layout::FLAG_COMPRESSIBLE != 0
            && self.pools.covered_by(start, end, |f| f & COMPRESSIBLE != 0)
        {
            return (Why::Compressed, Some(flags));
        }
        (Why::Copied, Some(flags))
    }

    /// Counts and names a decision (`NXKIT_DIAG=copyback`): the first one of each texture and each change.
    fn note(&self, id: TexId, rec: &TextureRecord, why: Why, flags: Option<u32>) {
        if !crate::diag("copyback") {
            return;
        }
        let mut decided = self.decided.lock().unwrap_or_else(|e| e.into_inner());
        if decided.get(&id) == Some(&why) || decided.len() >= 4 * LOG_TEXTURES {
            return;
        }
        decided.insert(id, why);
        {
            eprintln!(
                "DIAG copyback decision {id:?} fmt {:#x} {}x{} texture flags {:#x} at {:#x} size {:#x}: {} (pool flags {})",
                rec.desc.format,
                rec.desc.width,
                rec.desc.height,
                rec.desc.flags,
                rec.address.get(),
                rec.layout.size,
                why.name(),
                flags.map_or("none".to_owned(), |f| format!("{f:#x}")),
            );
        }
    }

    /// The textures decided so far by reason, for the summary.
    fn reasons(&self) -> String {
        let decided = self.decided.lock().unwrap_or_else(|e| e.into_inner());
        Why::ALL
            .iter()
            .map(|w| {
                format!(
                    "{} {}",
                    w.name(),
                    decided.values().filter(|v| *v == w).count()
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// True when an upload of `rec` reads guest memory that a copy-back of a recent batch writes, so the upload has
    /// to wait for the earlier work of the queue.
    pub(crate) fn upload_waits(&self, rec: &TextureRecord) -> bool {
        let (start, end) = (rec.address.get(), rec.end());
        self.recent.iter().any(|&(s, e, _)| s < end && start < e)
    }

    /// Notes the copy-backs of batch `batch` and forgets the ones that are surely done.
    pub(crate) fn submitted(&mut self, batch: u64, ranges: &[(u64, u64)]) {
        self.recent
            .retain(|&(_, _, b)| b.saturating_add(RECENT_BATCHES) > batch);
        self.recent
            .extend(ranges.iter().map(|&(s, e)| (s, e, batch)));
    }

    /// The GPU finished every submitted batch.
    pub(crate) fn idle(&mut self) {
        self.recent.clear();
    }

    /// Prints the texture the first time it is copied back (`NXKIT_DIAG=copyback`).
    pub(crate) fn log_first(
        &mut self,
        id: TexId,
        rec: &TextureRecord,
        res: &Res,
        flags: u32,
        dirty: &Dirty,
        bytes: u64,
    ) {
        if !crate::diag("copyback") || self.seen.len() >= LOG_TEXTURES || !self.seen.insert(id) {
            return;
        }
        let d = res.desc;
        let parts = match dirty {
            Dirty::All => "all".to_owned(),
            Dirty::Parts(v) => format!("{} of {}", v.len(), d.mip_levels * d.slice_count()),
        };
        eprintln!(
            "DIAG copyback first {id:?} {:?} {:?} {}x{}x{} levels {} layers {} {} at {:#x} size {:#x} pool flags {flags:#x} parts {parts} bytes {bytes}",
            d.format,
            d.kind,
            d.width,
            d.height,
            d.depth,
            d.mip_levels,
            d.array_layers,
            if rec.layout.linear { "pitch" } else { "block" },
            rec.address.get(),
            rec.layout.size,
        );
    }

    /// Prints the totals now and then, and a refused copy-back (`NXKIT_DIAG=copyback`).
    pub(crate) fn log_frame(&mut self, textures: u32, bytes: u64, refused: u32, over_budget: u32) {
        if textures == 0 && refused == 0 && over_budget == 0 {
            return;
        }
        self.frames += 1;
        self.textures += u64::from(textures);
        self.bytes += bytes;
        let n = self.frames;
        static LOSSES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let loss = (refused != 0 || over_budget != 0) && crate::diag::first(&LOSSES, 100);
        if crate::diag("copyback") && (n <= 20 || n.is_multiple_of(300) || loss) {
            eprintln!("DIAG copyback decisions: {}", self.reasons());
            eprintln!(
                "DIAG copyback frame {n}: {textures} textures {} KiB, refused {refused}, over budget {over_budget}; total {} textures {} MiB",
                bytes >> 10,
                self.textures,
                self.bytes >> 20
            );
        }
    }
}

#[cfg(test)]
mod tests;
