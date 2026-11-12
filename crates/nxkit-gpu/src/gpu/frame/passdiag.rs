//! The diagnostic text of a frame: the pass description of `passprof` and the tiny texture dump at the frame end.

use super::*;

impl Gpu {
    /// `NXKIT_DIAG=tiny` at the end of a sampled frame, after the GPU finished: the guest bytes of the tiny textures
    /// that were copied back, and the ranges of `NXKIT_DIAG_WATCH`.
    pub(super) fn tiny_frame_end(
        &self,
        view: &dyn EncodeView,
        textures: &FastMap<TexId, Res>,
        mirrored: &[TexId],
    ) {
        let base = self.guest.address_map().gpu_base();
        let read = |addr: u64, len: usize| {
            let mut buf = vec![0u8; len];
            self.guest
                .read(addr.wrapping_sub(base), &mut buf)
                .ok()
                .map(|()| buf)
        };
        for id in mirrored {
            let (Some(res), Ok(rec)) = (textures.get(id), view.record(*id)) else {
                continue;
            };
            if rec.layout.size == 0
                || rec.layout.size > crate::diag::TINY_MAX
                || !crate::diag::tiny_line()
            {
                continue;
            }
            if let Some(buf) = read(rec.address.get(), rec.layout.size as usize) {
                eprintln!(
                    "DIAG tiny copyback {id:?} {:?} {}x{} at 0x{:x} {}",
                    res.desc.format,
                    res.desc.width,
                    res.desc.height,
                    rec.address.get(),
                    crate::diag::tiny_words(&buf)
                );
            }
        }
        for &(addr, len) in crate::diag::watch_ranges() {
            if let Some(buf) = read(addr, len) {
                eprintln!(
                    "DIAG tiny watch 0x{addr:x} {}",
                    crate::diag::tiny_words(&buf)
                );
            }
        }
    }
}

/// `NXKIT_DIAG=passprof`: what a pass is, for the report.
pub(super) fn describe_pass(
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[crate::draws::Ordinal],
    textures: &FastMap<TexId, Res>,
    i: usize,
) -> String {
    use crate::detail::CopyKind;
    let p = &plan.passes[i];
    let n = p.cmds.len();
    let cmds = || {
        p.cmds
            .clone()
            .filter_map(|c| Some((frame.cmds.get(c as usize)?, ords.get(c as usize).copied()?)))
    };
    match p.kind {
        PassKind::Render => {
            let what = plan
                .attachments_of(p)
                .first()
                .and_then(|a| textures.get(&a.tex))
                .map_or_else(String::new, |r| {
                    format!(" {}x{} {:?}", r.desc.width, r.desc.height, r.desc.format)
                });
            let draws = cmds().filter(|(c, _)| matches!(c, Cmd::Draw { .. }));
            let count = draws.clone().count();
            let fs = draws
                .clone()
                .next()
                .and_then(|(_, o)| detail.draws.get(o.draw as usize))
                .map_or_else(
                    || "none".to_string(),
                    |d| {
                        d.program
                            .fragment
                            .as_deref()
                            .map_or_else(|| "depth-only".to_string(), str::to_string)
                    },
                );
            format!("render cmds {n} draws {count} fs {fs}{what}")
        }
        PassKind::Compute => {
            let ds: Vec<String> = cmds()
                .filter(|(c, _)| matches!(c, Cmd::Dispatch { .. }))
                .map(|(_, o)| {
                    detail.dispatches.get(o.dispatch as usize).map_or_else(
                        || "?".to_string(),
                        |d| {
                            let [x, y, z] = d.groups;
                            match d.indirect {
                                Some(a) => format!("{} indirect@{a:#x}", d.program),
                                None => format!("{} {x}x{y}x{z}", d.program),
                            }
                        },
                    )
                })
                .collect();
            let more = ds.len().saturating_sub(3);
            let mut s = format!(
                "compute cmds {n} [{}]",
                ds.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
            );
            if more > 0 {
                s.push_str(&format!(" +{more}"));
            }
            s
        }
        PassKind::Copy => {
            let items: Vec<String> = cmds()
                .map(|(c, o)| {
                    let d = detail.copies.get(o.copy as usize);
                    let [w, h, dp] = d.map_or([0; 3], |d| d.size);
                    match (c, d.map(|d| d.kind)) {
                        (Cmd::Copy { src, dst }, k) => format!(
                            "{} {w}x{h}x{dp} {src:?}->{dst:?}",
                            match k {
                                Some(CopyKind::TextureToTexture) => "tex-copy",
                                Some(CopyKind::BufferToTexture) => "buf-to-tex",
                                Some(CopyKind::TextureToBuffer) => "tex-to-buf",
                                Some(CopyKind::Clear) => "clear",
                                _ => "copy",
                            }
                        ),
                        (Cmd::BufferCopy, _) => d.map_or_else(
                            || "buffer".to_string(),
                            |d| match d.kind {
                                CopyKind::BufferFill => {
                                    format!("fill {} bytes @{:#x}", d.bytes, d.dst_addr)
                                }
                                _ => format!(
                                    "buffer-copy {} bytes {:#x}->{:#x}",
                                    d.bytes, d.src_addr, d.dst_addr
                                ),
                            },
                        ),
                        (Cmd::Downsample { src, dst }, _) => {
                            format!("downsample {w}x{h} {src:?}->{dst:?}")
                        }
                        _ => "other".to_string(),
                    }
                })
                .collect();
            let more = items.len().saturating_sub(3);
            let mut s = format!(
                "copy cmds {n} [{}]",
                items.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
            );
            if more > 0 {
                s.push_str(&format!(" +{more}"));
            }
            s
        }
        PassKind::CpuRead => {
            let items: Vec<String> = cmds()
                .filter_map(|(c, _)| match c {
                    Cmd::CpuRead(id) => Some(match textures.get(id) {
                        Some(r) => format!(
                            "{id:?} {}x{} {:?}",
                            r.desc.width, r.desc.height, r.desc.format
                        ),
                        None => format!("{id:?}"),
                    }),
                    _ => None,
                })
                .collect();
            format!("cpuread [{}]", items.join("; "))
        }
        _ => format!("other cmds {n}"),
    }
}
