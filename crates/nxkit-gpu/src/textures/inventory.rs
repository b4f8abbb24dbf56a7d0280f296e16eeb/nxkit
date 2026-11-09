//! The texture inventory for the memory report (`NXKIT_MEM_REPORT`, line `nxkit-mem tex`).
//!
//! Every [`SCAN_EVERY`](super::evict::SCAN_EVERY) frames, when the report is on, the store makes one pass over its
//! entries and publishes the totals here. The host reads them with [`latest`]. It measures only: nothing in the
//! encoder looks at it, and nothing runs per draw or per bind.
//!
//! What is counted. A Metal texture is one `generation`; several ids (texture objects of the game over the same
//! memory) may share it, and it is counted once, in exactly one [`Class`]. The bytes are `allocatedSize` of the Metal
//! texture. Not counted: the texture views (they share the storage of their texture, so only their number is kept),
//! and the textures outside the store (the staging textures of the scaled upload, the upload scratch, the native
//! plans). The host prints the process total beside the store total, so the difference shows. The block shadows own
//! storage and are counted. The `retired` textures are the ones the store just replaced or evicted and still holds
//! until the GPU is done; one of them may share storage with a live texture only when its id was made again before it
//! was dropped, which the totals count twice.
//!
//! The guest size of a texture is `TextureLayout::size` of its record when the Metal texture was made (`Res`). The
//! scale is the effective scale of that moment, 1 for an unscaled texture.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use nxkit_metal::TextureDesc;

use super::*;

/// The class of a Metal texture. A texture is in the first class that fits, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Tile memory only; it owns no storage, so its bytes are normally 0.
    Memoryless,
    /// A render target at any time.
    Attachment,
    /// A frame wrote it but it was never an attachment (also image atomics).
    GpuWritten,
    /// The CPU reads it back.
    CpuRead,
    /// Its size is the NVN size times a scale and it is none of the above.
    Scaled,
    /// The Metal format has another layout than the NVN format and it is none of the above.
    Widened,
    /// Everything else: a plain uploaded texture that the idle rule may drop.
    Sampled,
}

impl Class {
    /// The classes in the order of precedence, which is also the order of [`Inventory::classes`].
    pub const ALL: [Class; 7] = [
        Class::Memoryless,
        Class::Attachment,
        Class::GpuWritten,
        Class::CpuRead,
        Class::Scaled,
        Class::Widened,
        Class::Sampled,
    ];

    /// The name in the report.
    pub fn name(self) -> &'static str {
        match self {
            Class::Memoryless => "memoryless",
            Class::Attachment => "attachment",
            Class::GpuWritten => "gpu-written",
            Class::CpuRead => "cpu-read",
            Class::Scaled => "scaled",
            Class::Widened => "widened",
            Class::Sampled => "sampled",
        }
    }
}

/// The facts of `evict.rs` for one Metal texture, with the render target reasons apart.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Flags {
    pub(super) memoryless: bool,
    /// Some id of the texture is, or was, an attachment.
    pub(super) attachment: bool,
    /// Image atomics ran on it.
    pub(super) atomic: bool,
    pub(super) cpu_read: bool,
    pub(super) gpu_written: bool,
    pub(super) scaled: bool,
    pub(super) widened: bool,
}

/// The class of a texture with these facts.
pub(super) fn classify(f: &Flags) -> Class {
    if f.memoryless {
        Class::Memoryless
    } else if f.attachment {
        Class::Attachment
    } else if f.gpu_written || f.atomic {
        Class::GpuWritten
    } else if f.cpu_read {
        Class::CpuRead
    } else if f.scaled {
        Class::Scaled
    } else if f.widened {
        Class::Widened
    } else {
        Class::Sampled
    }
}

/// A count and the bytes of the things counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Total {
    /// How many.
    pub count: u64,
    /// Their bytes.
    pub bytes: u64,
}

impl Total {
    fn add(&mut self, bytes: u64) {
        self.count += 1;
        self.bytes += bytes;
    }
}

/// The totals of one class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassTotal {
    /// The Metal textures of the class and their `allocatedSize`.
    pub metal: Total,
    /// The sum of their sizes in guest memory (`TextureLayout::size`).
    pub guest_bytes: u64,
}

/// One of the largest Metal textures.
#[derive(Clone, Debug, PartialEq)]
pub struct Largest {
    /// The Metal format as `Debug` prints it.
    pub format: String,
    /// The width of level 0 in pixels (scaled, when the texture is scaled).
    pub width: u32,
    /// The height of level 0 in pixels.
    pub height: u32,
    /// The depth of a 3D texture, else the layers (6 for a cube).
    pub depth_or_layers: u32,
    /// The mip levels.
    pub mips: u32,
    /// The scale of the size, 1 when unscaled.
    pub scale: f32,
    /// The class.
    pub class: Class,
    /// The `allocatedSize`.
    pub bytes: u64,
    /// Frames since a frame used it last.
    pub idle: u64,
}

/// Live entries (ids) that share a guest range with others, and the Metal textures behind them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dup {
    /// The entries.
    pub entries: u64,
    /// The distinct Metal textures of those entries (fewer than `entries` when they share one).
    pub textures: u64,
    /// The `allocatedSize` of those textures.
    pub bytes: u64,
}

/// The idle buckets of the `sampled` class and of the attachment entries: used in the last 60 frames, in the last
/// 600, older.
pub const IDLE_EDGES: [u64; 2] = [60, 600];

/// The totals of one scan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inventory {
    /// The frame of the scan.
    pub frame: u64,
    /// The classes, in the order of [`Class::ALL`].
    pub classes: [ClassTotal; 7],
    /// The texture views in the caches (they own no storage; only their number is kept).
    pub views: u64,
    /// The block shadows of compressed attachments.
    pub shadows: Total,
    /// The textures that the store replaced or evicted and holds until the GPU is done.
    pub retired: Total,
    /// The Metal textures by format, `Debug` name, all of them, the largest bytes first.
    pub formats: Vec<(String, Total)>,
    /// The `sampled` textures by idle frames: at most 60, at most 600, older.
    pub idle: [Total; 3],
    /// The scaled textures (any class) and the bytes they would have at scale 1 (bytes divided by scale squared).
    pub scaled: Total,
    /// The bytes the scaled textures would have at scale 1.
    pub scaled_at_one: u64,
    /// The entries that share the start address and the whole description with another live entry.
    pub same_range: Dup,
    /// The entries whose guest range overlaps the range of another live entry.
    pub overlap: Dup,
    /// The part of `overlap` that are attachment entries (the id was an attachment).
    pub overlap_attachment: Dup,
    /// The part of `overlap` that are all other entries.
    pub overlap_other: Dup,
    /// The attachment entries (ids that were an attachment) by idle frames of their Metal texture, in the buckets of
    /// `idle`. The count is entries; the bytes are of the distinct Metal textures behind them.
    pub attachment_idle: [Total; 3],
    /// The distinct (format, width, height) among the attachment entries.
    pub attachment_shapes: u64,
    /// The Metal textures the supersede rule dropped so far and their bytes.
    pub superseded: Total,
    /// The ids that got a Metal texture again after their texture was superseded.
    pub remade: u64,
    /// The largest Metal textures, largest first (at most [`LARGEST`]).
    pub largest: Vec<Largest>,
}

/// How many of the largest textures are kept.
pub const LARGEST: usize = 8;

/// One Metal texture, for [`build`].
pub(super) struct Tex {
    pub(super) class: Class,
    pub(super) desc: TextureDesc,
    pub(super) bytes: u64,
    pub(super) guest: u64,
    pub(super) scale: f32,
    pub(super) idle: u64,
}

/// One live entry (id), for [`build`].
pub(super) struct Span {
    pub(super) address: u64,
    pub(super) size: u64,
    pub(super) desc: TextureDesc,
    pub(super) generation: u64,
    pub(super) bytes: u64,
    /// The id is in the attachment set of the store.
    pub(super) attachment: bool,
    /// Frames since a frame used the Metal texture of the entry last.
    pub(super) idle: u64,
}

fn sum_up(ids: &[usize], spans: &[Span]) -> Dup {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut d = Dup {
        entries: ids.len() as u64,
        ..Dup::default()
    };
    for &i in ids {
        if seen.insert(spans[i].generation) {
            d.textures += 1;
            d.bytes += spans[i].bytes;
        }
    }
    d
}

/// The entries that have the same start and description as another entry.
fn same_range(spans: &[Span]) -> Dup {
    let mut groups: HashMap<(u64, TextureDesc), Vec<usize>> = HashMap::new();
    for (i, s) in spans.iter().enumerate() {
        groups.entry((s.address, s.desc)).or_default().push(i);
    }
    let ids: Vec<usize> = groups
        .into_values()
        .filter(|g| g.len() > 1)
        .flatten()
        .collect();
    sum_up(&ids, spans)
}

/// The entries whose range `[address, address + size)` overlaps the range of another entry. Empty ranges are left out.
fn overlap(spans: &[Span]) -> Dup {
    sum_up(&overlapping(spans), spans)
}

/// The overlapping entries split into attachment entries and the others.
fn overlap_split(spans: &[Span]) -> (Dup, Dup) {
    let (a, o): (Vec<usize>, Vec<usize>) = overlapping(spans)
        .into_iter()
        .partition(|&i| spans[i].attachment);
    (sum_up(&a, spans), sum_up(&o, spans))
}

/// The attachment entries by idle bucket, and the number of distinct shapes among them.
fn attachments(spans: &[Span]) -> ([Total; 3], u64) {
    let mut idle = [Total::default(); 3];
    let mut seen: HashSet<u64> = HashSet::new();
    let mut shapes: HashSet<(nxkit_metal::Format, u32, u32)> = HashSet::new();
    for s in spans.iter().filter(|s| s.attachment) {
        let b = IDLE_EDGES
            .iter()
            .position(|&e| s.idle <= e)
            .unwrap_or(IDLE_EDGES.len());
        idle[b].count += 1;
        if seen.insert(s.generation) {
            idle[b].bytes += s.bytes;
        }
        shapes.insert((s.desc.format, s.desc.width, s.desc.height));
    }
    (idle, shapes.len() as u64)
}

/// The indices of the entries that overlap another entry.
fn overlapping(spans: &[Span]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..spans.len()).filter(|&i| spans[i].size > 0).collect();
    order.sort_by_key(|&i| spans[i].address);
    let end = |i: usize| spans[i].address.saturating_add(spans[i].size);
    let mut hit = vec![false; order.len()];
    let mut max_end = 0;
    for (k, &i) in order.iter().enumerate() {
        // An earlier entry reaches over my start.
        if k > 0 && max_end > spans[i].address {
            hit[k] = true;
        }
        // My end reaches over the start of the next one, which starts at or after mine.
        if let Some(&j) = order.get(k + 1)
            && spans[j].address < end(i)
        {
            hit[k] = true;
            hit[k + 1] = true;
        }
        max_end = max_end.max(end(i));
    }
    order
        .iter()
        .zip(&hit)
        .filter_map(|(&i, &h)| h.then_some(i))
        .collect()
}

/// The totals for these textures and entries.
pub(super) fn build(
    frame: u64,
    texs: &[Tex],
    spans: &[Span],
    views: u64,
    shadows: Total,
    retired: Total,
) -> Inventory {
    let mut inv = Inventory {
        frame,
        views,
        shadows,
        retired,
        ..Inventory::default()
    };
    let mut formats: HashMap<nxkit_metal::Format, Total> = HashMap::new();
    for t in texs {
        let c = &mut inv.classes[t.class as usize];
        c.metal.add(t.bytes);
        c.guest_bytes += t.guest;
        formats.entry(t.desc.format).or_default().add(t.bytes);
        if t.class == Class::Sampled {
            let b = IDLE_EDGES
                .iter()
                .position(|&e| t.idle <= e)
                .unwrap_or(IDLE_EDGES.len());
            inv.idle[b].add(t.bytes);
        }
        if t.scale != 1.0 && t.scale > 0.0 {
            inv.scaled.add(t.bytes);
            inv.scaled_at_one += (t.bytes as f64 / f64::from(t.scale * t.scale)).round() as u64;
        }
    }
    inv.formats = formats
        .into_iter()
        .map(|(f, t)| (format!("{f:?}"), t))
        .collect();
    inv.formats
        .sort_by(|a, b| b.1.bytes.cmp(&a.1.bytes).then_with(|| a.0.cmp(&b.0)));
    let mut big: Vec<&Tex> = texs.iter().collect();
    big.sort_by_key(|t| std::cmp::Reverse(t.bytes));
    inv.largest = big
        .into_iter()
        .take(LARGEST)
        .map(|t| Largest {
            format: format!("{:?}", t.desc.format),
            width: t.desc.width,
            height: t.desc.height,
            depth_or_layers: if t.desc.kind == TextureKind::D3 {
                t.desc.depth
            } else {
                t.desc.slice_count()
            },
            mips: t.desc.mip_levels,
            scale: t.scale,
            class: t.class,
            bytes: t.bytes,
            idle: t.idle,
        })
        .collect();
    inv.same_range = same_range(spans);
    inv.overlap = overlap(spans);
    (inv.overlap_attachment, inv.overlap_other) = overlap_split(spans);
    (inv.attachment_idle, inv.attachment_shapes) = attachments(spans);
    inv
}

static LATEST: Mutex<Option<Inventory>> = Mutex::new(None);

/// The inventory of the last scan, `None` before the first one or when the memory report is off.
pub fn latest() -> Option<Inventory> {
    LATEST
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// True when `NXKIT_MEM_REPORT` asks for the memory report (the same rule as the host: a number above 0).
pub(super) fn wanted() -> bool {
    std::env::var("NXKIT_MEM_REPORT")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .is_some_and(|s| s > 0.0)
}

impl TextureStore {
    /// Makes the inventory of the entries now and publishes it. One pass over the entries, a few allocations.
    pub(super) fn publish_inventory(&self) {
        struct G<'a> {
            res: &'a Res,
            flags: Flags,
        }
        let mut gens: FastMap<u64, G> = FastMap::default();
        for (id, r) in &self.entries {
            let g = gens.entry(r.generation).or_insert_with(|| G {
                res: r,
                flags: Flags {
                    memoryless: r.desc.storage == Storage::Memoryless,
                    gpu_written: self.usage.get(&r.generation).is_some_and(|u| u.gpu_written),
                    scaled: r.scaled,
                    widened: r.widened,
                    ..Flags::default()
                },
            });
            g.flags.attachment |= self.attachments.contains(id);
            g.flags.cpu_read |= self.pinned.contains(id);
            g.flags.atomic |= self.atomic.contains(id);
        }
        let mut bytes: FastMap<u64, u64> = FastMap::default();
        let texs: Vec<Tex> = gens
            .iter()
            .map(|(generation, g)| {
                let b = g.res.tex.allocated_bytes();
                bytes.insert(*generation, b);
                let last = self
                    .usage
                    .get(generation)
                    .map_or(self.frame, |u| u.last_frame);
                Tex {
                    class: classify(&g.flags),
                    desc: g.res.desc,
                    bytes: b,
                    guest: g.res.guest_size,
                    scale: g.res.scale,
                    idle: self.frame.saturating_sub(last),
                }
            })
            .collect();
        let spans: Vec<Span> = self
            .entries
            .iter()
            .map(|(id, r)| Span {
                address: r.guest_address,
                size: r.guest_size,
                desc: r.desc,
                generation: r.generation,
                bytes: bytes.get(&r.generation).copied().unwrap_or(0),
                attachment: self.attachments.contains(id),
                idle: self.frame.saturating_sub(
                    self.usage
                        .get(&r.generation)
                        .map_or(self.frame, |u| u.last_frame),
                ),
            })
            .collect();
        let total = |it: &mut dyn Iterator<Item = u64>| {
            let mut t = Total::default();
            it.for_each(|b| t.add(b));
            t
        };
        let shadows = total(
            &mut self
                .shadows
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .values()
                .map(Texture::allocated_bytes),
        );
        let retired = total(&mut self.retired.iter().map(Texture::allocated_bytes));
        let views = self
            .views
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len() as u64;
        let mut inv = build(self.frame, &texs, &spans, views, shadows, retired);
        (inv.superseded, inv.remade) = (self.sup.dropped, self.sup.remade);
        *LATEST.lock().unwrap_or_else(PoisonError::into_inner) = Some(inv);
    }
}

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod tests;
