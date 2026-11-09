//! The diagnostics of the encode (`NXKIT_DIAG`).

use super::*;

/// `NXKIT_DIAG=passes`: every 300 frames, the render passes of the frame by the number of draws and by the size of
/// their first attachment, to see which passes make the encoder boundaries of the GPU.
pub(super) fn pass_shape_diag(
    frame: &nxkit_nvn::frame::Frame,
    plan: &nxkit_nvn::passes::PassPlan,
    textures: &crate::draws::FastMap<nxkit_nvn::resource::TexId, Res>,
) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    if !crate::diag("passes") || N.fetch_add(1, Ordering::Relaxed) % 300 != 150 {
        return;
    }
    let mut by_draws = std::collections::BTreeMap::<&str, u32>::new();
    let mut by_size = std::collections::BTreeMap::<String, (u32, u32)>::new();
    for p in plan
        .passes
        .iter()
        .filter(|p| p.kind == nxkit_nvn::passes::PassKind::Render)
    {
        let cmds = &frame.cmds[p.cmds.start as usize..p.cmds.end as usize];
        let draws = cmds
            .iter()
            .filter(|c| matches!(c, nxkit_nvn::frame::Cmd::Draw { .. }))
            .count();
        let bucket = match draws {
            0 => "0 (clears)",
            1 => "1",
            2 => "2",
            3..=10 => "3-10",
            11..=100 => "11-100",
            _ => ">100",
        };
        *by_draws.entry(bucket).or_default() += 1;
        let size = plan
            .attachments_of(p)
            .first()
            .and_then(|a| textures.get(&a.tex))
            .map_or_else(
                || "?".to_string(),
                |r| format!("{}x{} {:?}", r.desc.width, r.desc.height, r.desc.format),
            );
        let e = by_size.entry(size).or_default();
        e.0 += 1;
        e.1 += draws as u32;
    }
    eprintln!("DIAG passes by draws: {by_draws:?}");
    if std::env::var_os("NXKIT_PASS_LIST").is_some() {
        for (i, p) in plan.passes.iter().enumerate() {
            let cmds = &frame.cmds[p.cmds.start as usize..p.cmds.end as usize];
            let draws = cmds
                .iter()
                .filter(|c| matches!(c, nxkit_nvn::frame::Cmd::Draw { .. }))
                .count();
            let atts: Vec<String> = plan
                .attachments_of(p)
                .iter()
                .filter_map(|a| {
                    textures.get(&a.tex).map(|r| {
                        format!(
                            "{}x{} {:?} {:?}/{:?}",
                            r.desc.width, r.desc.height, r.desc.format, a.load, a.store
                        )
                    })
                })
                .collect();
            eprintln!(
                "DIAG pass {i:3} {:?} {:?} draws {draws} cmds {} {atts:?}",
                p.kind,
                p.begin,
                cmds.len()
            );
        }
    }
    let mut v: Vec<_> = by_size.into_iter().collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.1.0));
    for (size, (n, draws)) in v.iter().take(20) {
        eprintln!("DIAG passes {n:4} with {draws:5} draws into {size}");
    }
}

/// `NXKIT_DIAG=hitch`: a line for each frame whose encode took long or that built pipelines or textures, with the wall
/// clock in milliseconds, to put it next to the late frames of `NXKIT_PACE_DIAG`.
pub(super) fn hitch_diag(s: &EncodeStats, draws: usize) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| crate::diag("hitch")) {
        return;
    }
    if s.encode_ns < 7_000_000 && s.pipeline_builds == 0 && s.textures_created == 0 {
        return;
    }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() % 1_000_000);
    let phases = PHASES.with(|p| *p.borrow());
    let chunks = CHUNKS.with(|c| {
        c.borrow()
            .iter()
            .map(|t| {
                format!(
                    "{}p +{:.1} {:.1}/{:.1}",
                    t.passes,
                    t.delay_ns as f64 / 1e6,
                    t.wall_ns as f64 / 1e6,
                    t.cpu_ns as f64 / 1e6
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    });
    let marks = MARKS.with(|m| {
        let m = m.borrow();
        let mut last = 0;
        m.iter()
            .map(|(n, at)| {
                let d = at - last;
                last = *at;
                format!("{n} {:.2}", d as f64 / 1e6)
            })
            .collect::<Vec<_>>()
            .join(", ")
    });
    eprintln!("DIAG hitch steps: {marks}");
    if s.chunks_ns > 5_000_000 {
        eprintln!("DIAG hitch chunks (passes +start wall/cpu ms): {chunks}");
    }
    eprintln!(
        "DIAG hitch t {ms} encode {:.2} ms chunks {:.2} ms create {:.2} ms upload {:.2} ms resolve {:.2} ms ring {:.2} ms copyback {:.2} ms (last commit {:.2} submit {:.2}) pipelines {} textures {} uploads {} copybacks {} passes {}+{} draws {draws}",
        s.encode_ns as f64 / 1e6,
        s.chunks_ns as f64 / 1e6,
        s.texture_create_ns as f64 / 1e6,
        s.upload_ns as f64 / 1e6,
        s.resolve_ns as f64 / 1e6,
        phases[0] as f64 / 1e6,
        phases[1] as f64 / 1e6,
        phases[2] as f64 / 1e6,
        phases[3] as f64 / 1e6,
        s.pipeline_builds,
        s.textures_created,
        s.uploads,
        s.copybacks,
        s.render_passes,
        s.compute_passes,
    );
}

/// One texture that a dispatch of a sampled frame read (`tex`) or wrote (`img`), for `NXKIT_DIAG=imgdump`.
pub(super) struct ImgDumpEntry {
    dispatch: usize,
    program: String,
    groups: [u32; 3],
    kind: &'static str,
    slot: u32,
    id: TexId,
    tex: Texture,
}

impl Gpu {
    /// `NXKIT_DIAG=imgdump`: every 120th frame notes the textures of its dispatches, and three frames later reads nine
    /// pixels of each and prints how many are not black, so a chain of compute passes shows where it turns black.
    pub(super) fn imgdump_step(
        &mut self,
        view: &dyn EncodeView,
        detail: &FrameDetail,
        textures: &FastMap<TexId, Res>,
    ) {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| crate::diag("imgdump")) {
            return;
        }
        self.frames_encoded += 1;
        let n = self.frames_encoded;
        while self.imgdump.front().is_some_and(|(f, _)| f + 3 <= n) {
            let Some((f, entries)) = self.imgdump.pop_front() else {
                break;
            };
            for e in entries {
                let d = e.tex.desc();
                let pts: Vec<[u32; 2]> = (1..4)
                    .flat_map(|j| (1..4).map(move |i| [d.width * i / 4, d.height * j / 4]))
                    .collect();
                let px = e.tex.debug_read_raw(&self.ctx, &pts);
                let lit = px.as_ref().map_or(0, |p| {
                    p.iter().filter(|v| v.iter().any(|b| *b != 0)).count()
                });
                eprintln!(
                    "DIAG imgdump frame {f} dispatch {} {} groups {:?} {} slot {} {:?} {}x{} {:?}: lit {lit}/9 first {:?}",
                    e.dispatch,
                    e.program,
                    e.groups,
                    e.kind,
                    e.slot,
                    e.id,
                    d.width,
                    d.height,
                    d.format,
                    px.as_ref().and_then(|p| p.get(4).cloned())
                );
            }
        }
        if !n.is_multiple_of(120) {
            return;
        }
        let mut entries = Vec::new();
        for (i, d) in detail.dispatches.iter().enumerate() {
            for (kind, list) in [("img", &d.images), ("tex", &d.textures)] {
                for b in list {
                    let Ok((slot, _)) = view.resolve(b.handle) else {
                        continue;
                    };
                    let Some(res) = textures.get(&slot.texture) else {
                        continue;
                    };
                    entries.push(ImgDumpEntry {
                        dispatch: i,
                        program: d.program.clone(),
                        groups: d.groups,
                        kind,
                        slot: b.slot,
                        id: slot.texture,
                        tex: res.tex.clone(),
                    });
                }
            }
        }
        self.imgdump.push_back((n, entries));
    }
}

/// `NXKIT_DIAG=kpass` (`docs/plans/kernel-passes.md`, step 0): every 300 frames, the render passes that could run as a
/// compute dispatch instead (one draw of at most 6 vertices or indices, one instance, no depth, stencil, blending or
/// alpha to coverage, one sample, colour targets that a kernel can write, at most 512x512), the runs of them in a row,
/// and why the other one-draw passes cannot.
pub(super) fn kpass_diag(
    frame: &Frame,
    plan: &PassPlan,
    detail: &FrameDetail,
    ords: &[crate::draws::Ordinal],
    textures: &FastMap<TexId, Res>,
) {
    use nxkit_metal::Format as F;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    if !crate::diag("kpass") || N.fetch_add(1, Ordering::Relaxed) % 300 != 150 {
        return;
    }
    const MAX_PX: u64 = 512 * 512;
    let mut why = std::collections::BTreeMap::<&str, u32>::new();
    let (mut candidates, mut renders, mut runs, mut in_run) = (0u32, 0u32, 0u32, false);
    let mut sizes = std::collections::BTreeMap::<String, u32>::new();
    for p in &plan.passes {
        let kind_render = p.kind == nxkit_nvn::passes::PassKind::Render;
        let mut ok = false;
        if kind_render {
            renders += 1;
            let cmds = &frame.cmds[p.cmds.start as usize..p.cmds.end as usize];
            let draws = cmds
                .iter()
                .filter(|c| matches!(c, Cmd::Draw { .. }))
                .count();
            let reason = if draws != 1 {
                Some(if draws == 0 {
                    "no draw"
                } else {
                    "several draws"
                })
            } else {
                let first = ords.get(p.cmds.start as usize).map_or(0, |o| o.draw) as usize;
                let atts = plan.attachments_of(p);
                let colors: Vec<&Res> = atts
                    .iter()
                    .filter(|a| matches!(a.slot, nxkit_nvn::passes::Slot::Color(_)))
                    .filter_map(|a| textures.get(&a.tex))
                    .collect();
                match detail.draws.get(first) {
                    None => Some("no detail"),
                    Some(d) => {
                        let small_geometry = match d.args {
                            DrawArgs::Arrays {
                                count, instances, ..
                            } => count <= 6 && instances <= 1,
                            DrawArgs::Indexed {
                                count, instances, ..
                            } => count <= 6 && instances <= 1,
                            _ => false,
                        };
                        let ds = &d.depth_stencil;
                        if !small_geometry {
                            Some("more than 6 vertices or instanced")
                        } else if ds.depth_test || ds.depth_write || ds.stencil_test {
                            Some("depth or stencil")
                        } else if d.blend.iter().any(|b| b.enable) {
                            Some("blending")
                        } else if d.raster.alpha_to_coverage {
                            Some("alpha to coverage")
                        } else if colors.is_empty() {
                            Some("no colour target")
                        } else if colors.iter().any(|r| {
                            r.desc.format.is_compressed()
                                || r.desc.format.is_depth()
                                || matches!(r.desc.format, F::Rgba8UnormSrgb | F::Bgra8UnormSrgb)
                        }) {
                            Some("format")
                        } else if colors
                            .iter()
                            .any(|r| u64::from(r.desc.width) * u64::from(r.desc.height) > MAX_PX)
                        {
                            Some("larger than 512x512")
                        } else {
                            if let Some(r) = colors.first() {
                                *sizes
                                    .entry(format!("{}x{}", r.desc.width, r.desc.height))
                                    .or_default() += 1;
                            }
                            None
                        }
                    }
                }
            };
            match reason {
                None => ok = true,
                Some(r) if draws == 1 => *why.entry(r).or_default() += 1,
                Some(r) => *why.entry(r).or_default() += 1,
            }
        }
        if ok {
            candidates += 1;
            if !in_run {
                runs += 1;
            }
            in_run = true;
        } else if kind_render {
            in_run = false;
        }
    }
    eprintln!(
        "DIAG kpass: {candidates} of {renders} render passes are candidates, in {runs} runs; the others: {why:?}; candidate targets: {sizes:?}"
    );
}
