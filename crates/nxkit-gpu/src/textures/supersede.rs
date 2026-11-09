//! Dropping the Metal textures whose guest memory another texture has overwritten.
//!
//! On the console a guest memory range exists once. When the GPU writes a texture, the bytes of every other texture
//! over the same memory are overwritten. The game (dynamic resolution) makes new texture objects over memory that
//! older objects used, with other sizes and formats, and the store keeps one Metal texture for each of them. A Metal
//! texture Y is superseded, and dropped, when all of these hold:
//!
//! 1. Another Metal texture X overlaps Y in guest memory.
//! 2. A frame wrote X after the last frame that used Y.
//! 3. Y was not used for at least [`MIN_FRAMES`] frames (frames may still be on the GPU).
//! 4. X is not one image of Y, or Y one image of X. A texture with several images (`multi`: more than one mip level
//!    or layer, a cube, a depth above 1) can have an image over the range of a smaller texture, and a write to that
//!    texture may have touched only one of its images. So Y stays when Y strictly contains X and Y is `multi`, and
//!    when X strictly contains Y and X is `multi`. In every other overlap (equal ranges, a partial overlap, a
//!    container that is one image) Y goes: a single image that was partly overwritten holds nothing useful. This
//!    way a smaller render target at the address of an older larger one (dynamic resolution) supersedes it.
//! 5. No id of Y is read back by the CPU. Memoryless textures hold no memory and are left alone.
//!
//! A texture that a frame used again after the write has a `last_frame` that is not older than the write and stays:
//! the game treats both as alive. `NXKIT_TEX_SUPERSEDE=0` turns the rule off.
//!
//! The decision is [`superseded`], a pure function over one [`Span`] per Metal texture. The store runs it in the scan
//! of `end_frame`, every [`SCAN_EVERY`] frames, as a batch.

use super::evict::{MIN_FRAMES, SCAN_EVERY};
use super::*;

/// How many following ranges of the sorted list the sweep compares with each range. Textures over one address come in
/// clusters (about 200 shapes is realistic), which this covers. At the bound the sweep stops for that range: the
/// pairs behind the bound are not looked at, those textures stay, and a later scan sees them again.
pub(super) const MAX_PARTNERS: usize = 256;

/// One Metal texture, for [`superseded`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Span {
    pub(super) generation: u64,
    /// The first guest byte.
    pub(super) start: u64,
    /// One after the last guest byte.
    pub(super) end: u64,
    /// The frame that used the texture last.
    pub(super) last_frame: u64,
    /// The last frame in which a frame wrote it.
    pub(super) written_frame: Option<u64>,
    /// Some id of the texture is read back by the CPU.
    pub(super) pinned: bool,
    /// The texture is memoryless.
    pub(super) memoryless: bool,
    /// The texture has more than one image: several mip levels or layers, a cube, or a depth above 1.
    pub(super) multi: bool,
}

/// A texture that goes, and the texture that overwrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Verdict {
    /// The generation that goes.
    pub(super) victim: u64,
    /// The generation with the latest write over it.
    pub(super) by: u64,
    /// The frame of that write.
    pub(super) written_frame: u64,
}

/// The frame of the write of `x` that supersedes `y` at frame `now`, if there is one.
fn overwrites(y: &Span, x: &Span, now: u64) -> Option<u64> {
    let written = x.written_frame?;
    // A memoryless texture never stores, so it overwrites nothing.
    if x.memoryless || y.memoryless || y.pinned || x.generation == y.generation {
        return None;
    }
    if now.saturating_sub(y.last_frame) < MIN_FRAMES || written <= y.last_frame {
        return None;
    }
    if x.start >= y.end || y.start >= x.end {
        return None;
    }
    let strict = |a: &Span, b: &Span| {
        a.start <= b.start && b.end <= a.end && (a.start, a.end) != (b.start, b.end)
    };
    // X may be one image of Y, or Y one image of X: only a part of the multi-image texture was touched.
    let part = (y.multi && strict(y, x)) || (x.multi && strict(x, y));
    (!part).then_some(written)
}

/// The textures that go at frame `now`, each with the texture that overwrote it last. A range that is empty never
/// takes part. The work is bounded by [`MAX_PARTNERS`] comparisons per range.
pub(super) fn superseded(spans: &[Span], now: u64) -> Vec<Verdict> {
    let mut order: Vec<usize> = (0..spans.len())
        .filter(|&i| spans[i].end > spans[i].start)
        .collect();
    order.sort_by_key(|&i| (spans[i].start, spans[i].generation));
    let mut best: Vec<Option<(u64, u64)>> = vec![None; spans.len()];
    for (k, &a) in order.iter().enumerate() {
        for &b in order[k + 1..].iter().take(MAX_PARTNERS) {
            // The list is sorted by start, so no later range reaches into `a` either.
            if spans[b].start >= spans[a].end {
                break;
            }
            for (y, x) in [(a, b), (b, a)] {
                if let Some(w) = overwrites(&spans[y], &spans[x], now)
                    && best[y].is_none_or(|(old, _)| w > old)
                {
                    best[y] = Some((w, spans[x].generation));
                }
            }
        }
    }
    best.iter()
        .enumerate()
        .filter_map(|(i, b)| {
            b.map(|(written_frame, by)| Verdict {
                victim: spans[i].generation,
                by,
                written_frame,
            })
        })
        .collect()
}

/// The state of the rule in the store.
#[derive(Default)]
pub(super) struct State {
    /// The rule is on (`NXKIT_TEX_SUPERSEDE` is not 0).
    enabled: bool,
    /// The Metal textures dropped so far and their bytes.
    pub(super) dropped: inventory::Total,
    /// The ids whose Metal texture was superseded and not made again yet.
    ids: FastSet<TexId>,
    /// The ids that got a Metal texture again after that.
    pub(super) remade: u64,
}

impl State {
    /// The state with the setting of `NXKIT_TEX_SUPERSEDE` (on unless it is "0").
    pub(super) fn from_env() -> Self {
        let off = std::env::var("NXKIT_TEX_SUPERSEDE").is_ok_and(|v| v.trim() == "0");
        Self {
            enabled: !off,
            ..Self::default()
        }
    }

    /// Forgets the id: the game made the texture again, so it is not "superseded and waiting" any more.
    pub(super) fn forget(&mut self, id: TexId) {
        self.ids.remove(&id);
    }
}

impl TextureStore {
    /// Drops the Metal textures that another texture overwrote (see the module). Runs every [`SCAN_EVERY`] frames.
    /// Returns how many Metal textures it dropped; they are in `retired`.
    pub(super) fn supersede_scan(&mut self) -> usize {
        if !self.sup.enabled || !self.frame.is_multiple_of(SCAN_EVERY) {
            return 0;
        }
        let mut spans: FastMap<u64, Span> = FastMap::default();
        let mut meta: FastMap<u64, (TexId, TextureDesc)> = FastMap::default();
        for (id, r) in &self.entries {
            if r.guest_address == 0 || r.guest_size == 0 {
                continue;
            }
            let u = self
                .usage
                .get(&r.generation)
                .copied()
                .unwrap_or(evict::Usage {
                    last_frame: self.frame,
                    ..evict::Usage::default()
                });
            let s = spans.entry(r.generation).or_insert(Span {
                generation: r.generation,
                start: r.guest_address,
                end: r.guest_address.saturating_add(r.guest_size),
                last_frame: u.last_frame,
                written_frame: u.written_frame,
                pinned: false,
                memoryless: r.desc.storage == Storage::Memoryless,
                multi: r.desc.mip_levels > 1
                    || r.desc.array_layers > 1
                    || r.desc.depth > 1
                    || matches!(r.desc.kind, TextureKind::Cube | TextureKind::CubeArray),
            });
            s.pinned |= self.pinned.contains(id);
            meta.entry(r.generation).or_insert((*id, r.desc));
        }
        let spans: Vec<Span> = spans.into_values().collect();
        let verdicts = superseded(&spans, self.frame);
        if verdicts.is_empty() {
            return 0;
        }
        let gone: FastSet<u64> = verdicts.iter().map(|v| v.victim).collect();
        let mut counted: FastSet<u64> = FastSet::default();
        for (id, generation, bytes) in self.drop_generations(&gone) {
            self.sup.ids.insert(id);
            if counted.insert(generation) {
                self.sup.dropped.count += 1;
                self.sup.dropped.bytes += bytes;
            }
        }
        for v in &verdicts {
            let Some(&(id, d)) = meta.get(&v.victim) else {
                continue;
            };
            if life_diag(d.format, d.width, d.height, d.array_layers) {
                eprintln!(
                    "DIAG life superseded {id:?} gen {} {:?} {:?} {}x{}x{} l{} by {:?} gen {} written frame {}",
                    v.victim,
                    d.kind,
                    d.format,
                    d.width,
                    d.height,
                    d.array_layers,
                    d.mip_levels,
                    meta.get(&v.by).map(|m| m.0),
                    v.by,
                    v.written_frame
                );
            }
        }
        counted.len()
    }

    /// Counts the id when it gets a Metal texture again after its texture was superseded.
    pub(super) fn remade_after_supersede(&mut self, id: TexId) {
        if self.sup.ids.remove(&id) {
            self.sup.remade += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A span `[start, end)` that was used last at `last` and written at `written`.
    fn span(generation: u64, start: u64, end: u64, last: u64, written: Option<u64>) -> Span {
        Span {
            generation,
            start,
            end,
            last_frame: last,
            written_frame: written,
            pinned: false,
            memoryless: false,
            multi: false,
        }
    }

    /// The generations that go.
    fn gone(spans: &[Span], now: u64) -> Vec<u64> {
        let mut g: Vec<u64> = superseded(spans, now).iter().map(|v| v.victim).collect();
        g.sort_unstable();
        g
    }

    const NOW: u64 = 1000;

    #[test]
    fn a_texture_written_over_goes() {
        let s = [
            span(1, 0x1000, 0x2000, 10, None),
            span(2, 0x1000, 0x1800, 50, Some(50)),
        ];
        let v = superseded(&s, NOW);
        assert_eq!(
            v,
            vec![Verdict {
                victim: 1,
                by: 2,
                written_frame: 50
            }]
        );
    }

    #[test]
    fn a_texture_used_after_the_write_stays() {
        let s = [
            span(1, 0x1000, 0x2000, 60, None),
            span(2, 0x1000, 0x2000, 50, Some(50)),
        ];
        assert!(gone(&s, NOW).is_empty());
        // Used in the frame of the write: also alive.
        let s = [
            span(1, 0x1000, 0x2000, 50, None),
            span(2, 0x1000, 0x2000, 50, Some(50)),
        ];
        assert!(gone(&s, NOW).is_empty());
    }

    #[test]
    fn a_recent_texture_stays() {
        let s = [
            span(1, 0x1000, 0x2000, 10, None),
            span(2, 0x1000, 0x2000, 20, Some(20)),
        ];
        assert_eq!(gone(&s, 10 + MIN_FRAMES - 1), Vec::<u64>::new());
        assert_eq!(gone(&s, 10 + MIN_FRAMES), vec![1]);
    }

    #[test]
    fn a_multi_image_container_of_the_writer_stays() {
        let mut y = span(1, 0x1000, 0x3000, 10, None);
        let x = span(2, 0x1800, 0x2000, 50, Some(50));
        // The writer can be one mip level or layer of Y.
        y.multi = true;
        assert!(gone(&[y, x], NOW).is_empty());
        // Y is one image: it was partly overwritten and goes (a smaller target at the old address).
        y.multi = false;
        assert_eq!(gone(&[y, x], NOW), vec![1]);
    }

    #[test]
    fn equal_ranges_go() {
        let s = [
            span(1, 0x1000, 0x2000, 10, None),
            span(2, 0x1000, 0x2000, 50, Some(50)),
        ];
        assert_eq!(gone(&s, NOW), vec![1]);
    }

    #[test]
    fn a_texture_inside_a_multi_image_writer_stays() {
        let y = span(1, 0x1400, 0x1800, 10, None);
        let mut x = span(2, 0x1000, 0x2000, 50, Some(50));
        // Y can be one image of X, and the write may have gone to another image.
        x.multi = true;
        assert!(gone(&[y, x], NOW).is_empty());
        // X is one image over all of Y: Y is overwritten.
        x.multi = false;
        assert_eq!(gone(&[y, x], NOW), vec![1]);
    }

    #[test]
    fn multi_images_with_equal_or_partial_ranges_go() {
        let mut y = span(1, 0x1000, 0x2000, 10, None);
        y.multi = true;
        let mut x = span(2, 0x1000, 0x2000, 50, Some(50));
        x.multi = true;
        assert_eq!(gone(&[y, x], NOW), vec![1]);
        let mut x = span(2, 0x1800, 0x2800, 50, Some(50));
        x.multi = true;
        assert_eq!(gone(&[y, x], NOW), vec![1]);
    }

    #[test]
    fn a_partial_overlap_goes() {
        let s = [
            span(1, 0x1000, 0x2000, 10, None),
            span(2, 0x1800, 0x2800, 50, Some(50)),
        ];
        assert_eq!(gone(&s, NOW), vec![1]);
    }

    #[test]
    fn a_pinned_or_memoryless_texture_stays() {
        let mut y = span(1, 0x1000, 0x2000, 10, None);
        let x = span(2, 0x1000, 0x2000, 50, Some(50));
        y.pinned = true;
        assert!(gone(&[y, x], NOW).is_empty());
        y.pinned = false;
        y.memoryless = true;
        assert!(gone(&[y, x], NOW).is_empty());
        // A memoryless writer overwrites nothing.
        let y = span(1, 0x1000, 0x2000, 10, None);
        let mut x = x;
        x.memoryless = true;
        assert!(gone(&[y, x], NOW).is_empty());
    }

    #[test]
    fn ranges_without_overlap_stay() {
        let s = [
            span(1, 0x1000, 0x2000, 10, None),
            span(2, 0x3000, 0x4000, 50, Some(50)),
        ];
        assert!(gone(&s, NOW).is_empty());
    }

    #[test]
    fn ranges_that_only_touch_do_not_overlap() {
        let s = [
            span(1, 0x1000, 0x2000, 10, None),
            span(2, 0x2000, 0x3000, 50, Some(50)),
        ];
        assert!(gone(&s, NOW).is_empty());
    }

    #[test]
    fn a_texture_never_written_supersedes_nothing() {
        let s = [
            span(1, 0x1000, 0x2000, 10, None),
            span(2, 0x1000, 0x2000, 50, None),
        ];
        assert!(gone(&s, NOW).is_empty());
    }

    #[test]
    fn empty_ranges_never_take_part() {
        let s = [
            span(1, 0x1000, 0x1000, 10, None),
            span(2, 0x1000, 0x2000, 50, Some(50)),
        ];
        assert!(gone(&s, NOW).is_empty());
    }

    #[test]
    fn a_chain_leaves_the_newest() {
        let s = [
            span(1, 0x1000, 0x2000, 10, Some(10)),
            span(2, 0x1000, 0x2000, 20, Some(20)),
            span(3, 0x1000, 0x2000, 30, Some(30)),
        ];
        let v = superseded(&s, NOW);
        assert_eq!(gone(&s, NOW), vec![1, 2]);
        // The latest write wins the credit.
        assert!(v.iter().any(|v| v.victim == 1 && v.by == 3));
        assert!(v.iter().any(|v| v.victim == 2 && v.by == 3));
    }

    #[test]
    fn a_big_cluster_stays_bounded() {
        let mut s: Vec<Span> = (0..(MAX_PARTNERS as u64 * 3))
            .map(|g| span(g + 1, 0x1000, 0x2000, 10, None))
            .collect();
        s.push(span(9999, 0x1000, 0x2000, 50, Some(50)));
        let n = gone(&s, NOW).len();
        // Some of the cluster goes, never more than the textures that were there.
        assert!(n > 0 && n <= MAX_PARTNERS * 3);
    }
}
