//! The commands of a frame that write a texture or its memory, for [`super`].

use super::*;
use crate::detail::{CopyDetail, DrawDetail};
use nxkit_nvn::frame::RtSet;
use nxkit_nvn::resource::TextureRecord;

/// The lines of one list that are printed; the rest is counted.
const LINES: usize = 24;

/// A frame as the report needs it: the commands, the detail, and what the textures and image handles meant then.
pub(super) struct Snap {
    pub(super) frame: Frame,
    pub(super) detail: FrameDetail,
    pub(super) recs: FastMap<TexId, TextureRecord>,
    pub(super) images: FastMap<u64, TexId>,
}

impl Snap {
    pub(super) fn of(cx: &PrepareCx<'_>) -> Self {
        let mut recs = FastMap::default();
        let mut note = |id: TexId| {
            if let Ok(r) = cx.view.record(id) {
                recs.insert(id, *r);
            }
        };
        for c in &cx.frame.cmds {
            match *c {
                Cmd::SetRenderTargets(RtSet { color, depth, .. }) => {
                    color
                        .iter()
                        .flatten()
                        .chain(depth.iter())
                        .for_each(|i| note(*i));
                }
                Cmd::Copy { src, dst } => src.into_iter().chain(dst).for_each(&mut note),
                Cmd::Downsample { src, dst } => [src, dst].into_iter().for_each(&mut note),
                Cmd::Dispatch { writes, .. } => {
                    cx.frame.list(writes).iter().for_each(|i| note(*i));
                }
                _ => {}
            }
        }
        let mut images = FastMap::default();
        let draws = cx.detail.draws.iter().flat_map(|d| d.images.iter());
        let disps = cx.detail.dispatches.iter().flat_map(|d| d.images.iter());
        for b in draws.chain(disps) {
            if let Some((id, _, _)) = cx.resolve(b.handle) {
                images.insert(b.handle, id);
                note(id);
            }
        }
        Self {
            frame: cx.frame.clone(),
            detail: cx.detail.clone(),
            recs,
            images,
        }
    }

    pub(super) fn range(&self, id: TexId) -> Option<(u64, u64)> {
        self.recs.get(&id).map(|r| (r.address.get(), r.end()))
    }

    pub(super) fn name(&self, id: TexId) -> String {
        match self.recs.get(&id) {
            Some(r) => format!(
                "texture {} {:#x}..{:#x} {}x{} format {}",
                id.0,
                r.address.get(),
                r.end(),
                r.desc.width,
                r.desc.height,
                r.desc.format
            ),
            None => format!("texture {} (no record)", id.0),
        }
    }
}

/// The storage range of a texture of the dispatch, and the texture itself.
pub(super) struct Target {
    pub(super) id: TexId,
    pub(super) lo: u64,
    pub(super) hi: u64,
}

pub(super) fn meets(a: (u64, u64), t: &Target) -> bool {
    a.0 < t.hi && t.lo < a.1
}

/// The lines of one list: the first [`LINES`] and the count of the others.
#[derive(Default)]
pub(super) struct Lines {
    pub(super) lines: Vec<String>,
    pub(super) more: usize,
}

impl Lines {
    fn add(&mut self, line: String) {
        if self.lines.len() < LINES {
            self.lines.push(line);
        } else {
            self.more += 1;
        }
    }
}

/// What a draw or dispatch binds that lies over the target: images and storage buffers.
fn bound_over(s: &Snap, t: &Target, d: Bound<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for b in d.images {
        let hit = s.images.get(&b.handle).copied();
        if hit.and_then(|id| s.range(id)).is_some_and(|r| meets(r, t)) {
            let name = hit.map_or_else(String::new, |id| s.name(id));
            out.push(format!("{:?} image slot {} is {name}", b.stage, b.slot));
        }
    }
    for u in d.sbufs {
        if meets((u.address, u.address + u64::from(u.size)), t) {
            let (a, n) = (u.address, u.size);
            out.push(format!(
                "{:?} storage buffer slot {} {a:#x}+{n:#x}",
                u.stage, u.slot
            ));
        }
    }
    out
}

/// The images and storage buffers of one draw or dispatch.
pub(super) struct Bound<'a> {
    pub(super) images: &'a [crate::detail::TextureBind],
    sbufs: &'a [UboDetail],
}

fn program_of(d: &DrawDetail) -> String {
    let fs = d.program.fragment.as_deref().unwrap_or("-");
    format!("vs {} fs {fs}", d.program.vertex)
}

/// The bytes a copy writes, as far as its record says.
fn copy_len(c: &CopyDetail) -> u64 {
    c.image_stride
        .max(c.row_stride.saturating_mul(u64::from(c.size[1])))
        .max(1)
}

/// Every command of the frame that writes the target or its memory.
pub(super) fn writers(s: &Snap, t: &Target) -> Lines {
    let mut out = Lines::default();
    let mut rt: Option<String> = None;
    let (mut nd, mut ni, mut nc) = (0usize, 0usize, 0usize);
    for (i, c) in s.frame.cmds.iter().enumerate() {
        match *c {
            Cmd::SetRenderTargets(set) => {
                let slots = set
                    .color
                    .iter()
                    .enumerate()
                    .map(|(k, v)| (format!("colour {k}"), *v));
                let all = slots.chain([("depth".to_string(), set.depth)]);
                let hit: Vec<String> = all
                    .filter_map(|(n, v)| Some((n, v?)))
                    .filter(|(_, id)| s.range(*id).is_some_and(|r| meets(r, t)))
                    .map(|(n, id)| format!("{n} = {}", s.name(id)))
                    .collect();
                rt = (!hit.is_empty()).then(|| hit.join(", "));
                if let Some(w) = &rt {
                    out.add(format!("cmd {i}: render targets set, {w}"));
                }
            }
            Cmd::ClearColor { slot, full } if rt.is_some() => {
                out.add(format!(
                    "cmd {i}: clear of colour slot {slot} (full {full}) of an attachment"
                ));
            }
            Cmd::Discard { color, depth } if rt.is_some() => {
                out.add(format!(
                    "cmd {i}: discard (colour mask {color:#x}, depth {depth}) of an attachment"
                ));
            }
            Cmd::ClearDepthStencil { .. } if rt.is_some() => {
                out.add(format!(
                    "cmd {i}: clear of depth and stencil of an attachment"
                ));
            }
            Cmd::Draw { .. } => {
                nd += 1;
                let Some(d) = s.detail.draws.get(nd - 1) else {
                    continue;
                };
                if let Some(w) = &rt {
                    out.add(format!(
                        "cmd {i}: draw {} into {w}: {}",
                        nd - 1,
                        program_of(d)
                    ));
                }
                let b = bound_over(
                    s,
                    t,
                    Bound {
                        images: &d.images,
                        sbufs: &d.sbufs,
                    },
                );
                if !b.is_empty() {
                    out.add(format!(
                        "cmd {i}: draw {} binds {}: {}",
                        nd - 1,
                        b.join(", "),
                        program_of(d)
                    ));
                }
            }
            Cmd::Dispatch { writes, .. } => {
                ni += 1;
                let Some(d) = s.detail.dispatches.get(ni - 1) else {
                    continue;
                };
                let mut b = bound_over(
                    s,
                    t,
                    Bound {
                        images: &d.images,
                        sbufs: &d.sbufs,
                    },
                );
                for id in s.frame.list(writes) {
                    if s.range(*id).is_some_and(|r| meets(r, t)) {
                        b.push(format!("writes {}", s.name(*id)));
                    }
                }
                if !b.is_empty() {
                    let at = format!("cmd {i}: dispatch {} program {}", ni - 1, d.program);
                    out.add(format!("{at} {:?} binds {}", d.groups, b.join(", ")));
                }
            }
            Cmd::Copy { src, dst } => {
                nc += 1;
                let Some(c) = s.detail.copies.get(nc - 1) else {
                    continue;
                };
                let by_addr = c.dst_addr != 0 && meets((c.dst_addr, c.dst_addr + copy_len(c)), t);
                let by_tex = dst.is_some_and(|d| s.range(d).is_some_and(|r| meets(r, t)));
                if by_addr || by_tex {
                    out.add(copy_line(i, c, src, dst));
                }
            }
            Cmd::Downsample { dst, .. } => {
                nc += 1;
                if s.range(dst).is_some_and(|r| meets(r, t)) {
                    out.add(format!("cmd {i}: downsample into {}", s.name(dst)));
                }
            }
            _ => {}
        }
    }
    out
}

fn copy_line(i: usize, c: &CopyDetail, src: Option<TexId>, dst: Option<TexId>) -> String {
    format!(
        "cmd {i}: copy {:?} src {:#x} (texture {:?}) -> dst {:#x} (texture {:?}) box {:?} row stride {} image stride {}",
        c.kind,
        c.src_addr,
        src.map(|t| t.0),
        c.dst_addr,
        dst.map(|t| t.0),
        c.size,
        c.row_stride,
        c.image_stride
    )
}

/// The copies of the frame whose destination is nearest to the target, in case the picture goes somewhere else.
pub(super) fn nearest_copies(s: &Snap, t: &Target) -> String {
    let mut all: Vec<(u64, &CopyDetail)> = s
        .detail
        .copies
        .iter()
        .filter(|c| c.dst_addr != 0)
        .map(|c| (c.dst_addr.abs_diff(t.lo), c))
        .collect();
    all.sort_by_key(|(d, _)| *d);
    let top: Vec<String> = all
        .iter()
        .take(4)
        .map(|(_, c)| format!("{:?} -> {:#x}", c.kind, c.dst_addr))
        .collect();
    format!(
        "{} copies in the frame, nearest destinations: {}",
        all.len(),
        top.join("; ")
    )
}
