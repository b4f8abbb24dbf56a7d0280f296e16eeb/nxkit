//! Dropping the Metal textures that nothing has used for a while.
//!
//! Every guest texture has a private Metal copy of about its size, and the guest copy stays in guest memory. A texture
//! that the game uploaded once and does not draw any more holds that Metal copy for nothing. The store notes in which
//! frame each Metal texture was last used, and every [`SCAN_EVERY`] frames it drops the ones that were not used for
//! `NXKIT_TEX_EVICT` frames (default 600, about 10 s; 0 turns it off), when the guest copy is the current one. The
//! texture is made again, and uploaded again, when the game uses it later.
//!
//! A texture whose Metal copy may be newer than the guest copy never goes: a render target (an attachment at any
//! time), a texture that image atomics ran on, one that the CPU reads back, one that any frame wrote (`gpu_written`),
//! one that is memoryless, widened (the upload does not handle it) or scaled. The frames of the last
//! [`MIN_FRAMES`] frames may still be on the GPU, and the command buffers keep their textures alive, so a texture
//! that is dropped here lives on until the last of them is done.

use super::*;

/// The frames between two scans of the store.
pub(super) const SCAN_EVERY: u64 = 60;

/// The least number of idle frames that the setting may have: the ring runs up to this many frames ahead.
pub(super) const MIN_FRAMES: u64 = 8;

/// The default of `NXKIT_TEX_EVICT`.
const DEFAULT_FRAMES: u64 = 600;

/// The idle frames after which a texture goes, from the text of `NXKIT_TEX_EVICT`. None for "0" (off). An unset
/// variable gives the default, a value that is not a number too. A small number is raised to [`MIN_FRAMES`].
pub(super) fn parse_frames(text: Option<&str>) -> Option<u64> {
    match text.map(str::trim).map(str::parse::<u64>) {
        Some(Ok(0)) => None,
        Some(Ok(n)) => Some(n.max(MIN_FRAMES)),
        _ => Some(DEFAULT_FRAMES),
    }
}

pub(super) fn frames_from_env() -> Option<u64> {
    parse_frames(std::env::var("NXKIT_TEX_EVICT").ok().as_deref())
}

/// What the store knows about the use of one Metal texture.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Usage {
    /// The frame that used it last.
    pub(super) last_frame: u64,
    /// A frame wrote it (the table said `GpuNewer` or a pass wrote it) since it was made.
    pub(super) gpu_written: bool,
}

/// What [`evictable`] decides on, for one Metal texture.
#[derive(Clone, Copy, Debug)]
pub(super) struct Facts {
    pub(super) usage: Usage,
    /// Some id of the texture is, or was, an attachment, a CPU-read texture or an image atomic target.
    pub(super) render_target_like: bool,
    pub(super) memoryless: bool,
    pub(super) widened: bool,
    pub(super) scaled: bool,
}

/// Whether the texture can go at frame `now`, when textures idle for `after` frames go.
pub(super) fn evictable(now: u64, after: u64, f: &Facts) -> bool {
    now.saturating_sub(f.usage.last_frame) >= after
        && !f.usage.gpu_written
        && !f.render_target_like
        && !f.memoryless
        && !f.widened
        && !f.scaled
}

impl TextureStore {
    /// Notes that frame `self.frame` uses the Metal texture `generation`. `written` says that the table has the GPU
    /// copy as the newer one.
    pub(super) fn used(&mut self, generation: u64, written: bool) {
        let u = self.usage.entry(generation).or_default();
        u.last_frame = self.frame;
        u.gpu_written |= written;
    }

    /// Notes that a frame wrote the texture `id`.
    pub(crate) fn mark_written(&mut self, id: TexId) {
        if let Some(r) = self.entries.get(&id) {
            self.usage.entry(r.generation).or_default().gpu_written = true;
        }
    }

    /// Ends a frame. Every [`SCAN_EVERY`] frames it drops the textures that were idle for long enough. Returns how
    /// many Metal textures it dropped. The textures are in `retired`; the caller drops them with `take_retired`.
    pub(crate) fn end_frame(&mut self) -> usize {
        self.frame += 1;
        let dropped = self.evict_scan();
        // The memory report wants the inventory after the scan, also when eviction is off.
        if self.inventory && self.frame.is_multiple_of(SCAN_EVERY) {
            self.publish_inventory();
        }
        dropped
    }

    fn evict_scan(&mut self) -> usize {
        let Some(after) = self.evict_after else {
            return 0;
        };
        if !self.frame.is_multiple_of(SCAN_EVERY) {
            return 0;
        }
        let now = self.frame;
        let mut keep: FastSet<u64> = FastSet::default();
        let mut idle: FastMap<u64, Facts> = FastMap::default();
        for (id, r) in &self.entries {
            let blocked = self.attachments.contains(id)
                || self.pinned.contains(id)
                || self.atomic.contains(id);
            let f = idle.entry(r.generation).or_insert(Facts {
                usage: self.usage.get(&r.generation).copied().unwrap_or(Usage {
                    last_frame: now,
                    gpu_written: false,
                }),
                render_target_like: false,
                memoryless: r.desc.storage == Storage::Memoryless,
                widened: r.widened,
                scaled: r.scaled,
            });
            f.render_target_like |= blocked;
        }
        for (generation, f) in &idle {
            if !evictable(now, after, f) {
                keep.insert(*generation);
            }
        }
        let mut dropped: FastSet<u64> = FastSet::default();
        let mut ids = Vec::new();
        for (id, r) in &self.entries {
            if !keep.contains(&r.generation) {
                ids.push(*id);
            }
        }
        for id in ids {
            let Some(r) = self.entries.remove(&id) else {
                continue;
            };
            if dropped.insert(r.generation) {
                nxkit_metal::texture_evicted(r.tex.allocated_bytes());
            }
            self.evicted_ids.insert(id);
            self.retired.push(r.tex);
        }
        self.usage.retain(|g, _| !dropped.contains(g));
        if !dropped.is_empty() {
            // The alias map and the views hold the texture too.
            self.trim_views();
        }
        dropped.len()
    }

    /// Counts the re-upload of a texture that went, when `id` gets a Metal texture again.
    pub(super) fn made_again(&mut self, id: TexId) {
        if self.evicted_ids.remove(&id) {
            nxkit_metal::texture_reuploaded();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle(last_frame: u64) -> Facts {
        Facts {
            usage: Usage {
                last_frame,
                gpu_written: false,
            },
            render_target_like: false,
            memoryless: false,
            widened: false,
            scaled: false,
        }
    }

    #[test]
    fn the_setting() {
        assert_eq!(parse_frames(None), Some(600));
        assert_eq!(parse_frames(Some("0")), None);
        assert_eq!(parse_frames(Some(" 0 ")), None);
        assert_eq!(parse_frames(Some("120")), Some(120));
        assert_eq!(parse_frames(Some("2")), Some(MIN_FRAMES));
        assert_eq!(parse_frames(Some("x")), Some(600));
    }

    #[test]
    fn an_idle_texture_goes() {
        assert!(evictable(1000, 600, &idle(300)));
        assert!(evictable(1000, 600, &idle(400)));
    }

    #[test]
    fn a_recent_texture_stays() {
        assert!(!evictable(1000, 600, &idle(401)));
        assert!(!evictable(1000, 600, &idle(1000)));
        assert!(!evictable(10, 600, &idle(0)));
    }

    #[test]
    fn a_texture_the_gpu_wrote_stays() {
        let mut f = idle(0);
        f.usage.gpu_written = true;
        assert!(!evictable(10_000, 600, &f));
    }

    #[test]
    fn render_targets_and_special_textures_stay() {
        for set in [
            |f: &mut Facts| f.render_target_like = true,
            |f: &mut Facts| f.memoryless = true,
            |f: &mut Facts| f.widened = true,
            |f: &mut Facts| f.scaled = true,
        ] {
            let mut f = idle(0);
            set(&mut f);
            assert!(!evictable(10_000, 600, &f));
        }
    }
}
