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
//!
//! `NXKIT_COPYBACK=0` turns it off. `NXKIT_COPYBACK_MAX_MB` caps the storage copied back per frame (128 MiB).
//! `NXKIT_DIAG=copyback` names each texture the first time it is copied back, and prints a summary now and then.

use std::sync::OnceLock;

use nxkit_nvn::format::{Aspects, format_info};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::resource::{TexId, TextureRecord};

use crate::draws::FastSet;
use crate::poolreg::Registry;
use crate::textures::Res;
use crate::upload::Dirty;

/// `NVN_MEMORY_POOL_FLAGS_CPU_NO_ACCESS_BIT`.
pub(crate) const CPU_NO_ACCESS: u32 = 0x1;

/// `NVN_MEMORY_POOL_FLAGS_VIRTUAL_BIT`: a window of a virtual pool, not a pool with storage of its own.
pub(crate) const VIRTUAL: u32 = 0x200;

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
}

impl Why {
    const ALL: [Why; 8] = [
        Why::Copied,
        Why::Off,
        Why::FrameLocal,
        Why::Presented,
        Why::Format,
        Why::NoPool,
        Why::PoolNoCpu,
        Why::WindowNoCpu,
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
        }
    }
}

/// The batches after which a copy-back is surely done on the GPU, even without a wait.
const RECENT_BATCHES: u64 = 8;

/// The most textures that `NXKIT_DIAG=copyback` names.
const LOG_TEXTURES: usize = 300;

/// False when `NXKIT_COPYBACK=0`.
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NXKIT_COPYBACK").map_or(true, |v| v.trim() != "0"))
}

/// The storage bytes that one frame may copy back.
pub(crate) fn budget() -> u64 {
    static MB: OnceLock<u64> = OnceLock::new();
    *MB.get_or_init(|| {
        std::env::var("NXKIT_COPYBACK_MAX_MB")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(128)
    }) << 20
}

/// What the encoder knows for the copy-back: the memory pools with their flags, and the copy-backs in flight.
#[derive(Debug, Default)]
pub(crate) struct CopyBack {
    /// The memory pools with their `NVNmemoryPoolFlags`. Every pool stays until its removal; pools may overlap.
    pools: Registry<u32>,
    /// Textures the game presented. They are never candidates.
    presented: FastSet<TexId>,
    /// The storage ranges copied back in the last batches: (start, end, batch).
    recent: Vec<(u64, u64, u64)>,
    /// The textures that the diagnostic named.
    seen: FastSet<TexId>,
    /// The last decision for each texture (`NXKIT_DIAG=copyback` only). `candidate` takes `&self`.
    decided: std::sync::Mutex<crate::draws::FastMap<TexId, Why>>,
    /// Frames with a copy-back, textures and bytes copied back, for the diagnostic.
    frames: u64,
    textures: u64,
    bytes: u64,
}

impl CopyBack {
    /// A memory pool of the game at GPU address `start`. It is added to the pools it overlaps; none is forgotten.
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
                "DIAG copyback decision {id:?} fmt {:#x} {}x{} at {:#x} size {:#x}: {} (pool flags {})",
                rec.desc.format,
                rec.desc.width,
                rec.desc.height,
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
mod tests {
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
}
