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

    /// The pool flags of a texture that is copied back after a frame wrote it, or `None` when it is not copied back:
    /// a texture of another kind than plain colour, one that lives in a pool the CPU cannot access or in no known
    /// pool, one whose contents die with the frame, or one that the game presents.
    pub(crate) fn candidate(&self, id: TexId, rec: &TextureRecord) -> Option<u32> {
        if !enabled() || rec.frame_local || self.presented.contains(&id) {
            return None;
        }
        let info = format_info(rec.desc.format).ok()?;
        if info.aspects != Aspects::Color || info.block_w > 1 || info.block_h > 1 {
            return None;
        }
        let flags = self.pool_flags(rec.address.get(), rec.end())?;
        (flags & CPU_NO_ACCESS == 0).then_some(flags)
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
