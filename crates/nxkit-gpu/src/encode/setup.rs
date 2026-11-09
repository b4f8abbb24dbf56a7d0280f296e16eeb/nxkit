//! The setup of a render pass: attachments, load and store actions and the clear values.

use super::*;

pub(super) struct Setup {
    pub(super) desc: RenderPassDesc,
    pub(super) pc: PassCtx,
}

pub(super) fn load(a: LoadAction, memoryless: bool) -> MLoad {
    match a {
        LoadAction::Clear => MLoad::Clear,
        LoadAction::Load if !memoryless => MLoad::Load,
        _ => MLoad::DontCare,
    }
}

pub(super) fn store(a: StoreAction, memoryless: bool) -> MStore {
    if memoryless || a == StoreAction::DontCare {
        MStore::DontCare
    } else {
        MStore::Store
    }
}

/// The clear values that a pass starts with. The analysis cut the pass at the first full clear of a slot, so the
/// command is inside the pass.
pub(super) fn clear_of(sh: &Shared<'_>, pass: &Pass, slot: Slot) -> ClearDetail {
    for i in pass.cmds.clone() {
        let hit = match (sh.frame.cmds.get(i as usize), slot) {
            (
                Some(Cmd::ClearColor {
                    slot: s,
                    full: true,
                }),
                Slot::Color(n),
            ) => *s == n,
            (Some(Cmd::ClearDepthStencil { full: true }), Slot::Depth) => true,
            _ => false,
        };
        if hit {
            let at = sh.ordinals.get(i as usize).map_or(0, |o| o.clear as usize);
            return sh.detail.clears.get(at).copied().unwrap_or_default();
        }
    }
    ClearDetail::default()
}

/// True when the first draw of the pass visibly paints every texel of colour slot `slot` of an attachment of
/// `size` (after the scale `scale`): a full-screen triangle (or quad) with no vertex input, a viewport and a scissor
/// that hold the target, all four channels written, no blending, no depth or stencil test, no alpha to coverage.
/// Then the old contents are never read and a `Load` is wasted. The shader could still discard a fragment, and the
/// geometry of a vertex shader without input could still be small, so only the `coverload` knob uses this.
pub(super) fn first_draw_covers(
    sh: &Shared<'_>,
    pass: &Pass,
    slot: u8,
    size: [u32; 2],
    scale: f32,
) -> bool {
    let Some(d) = draw_range(sh, pass)
        .next()
        .and_then(|n| sh.detail.draws.get(n as usize))
    else {
        return false;
    };
    let vertices_ok = matches!(
        (d.topology, d.args),
        (
            4,
            DrawArgs::Arrays {
                count: 3 | 6,
                instances: 1,
                ..
            }
        ) | (
            5,
            DrawArgs::Arrays {
                count: 4,
                instances: 1,
                ..
            }
        )
    );
    let [x, y, w, h] = d.dynamic.viewport.map(|v| v * scale);
    let holds = |x0: f32, y0: f32, w0: f32, h0: f32| {
        x0 <= 0.5 && y0 <= 0.5 && x0 + w0 >= size[0] as f32 - 0.5 && y0 + h0 >= size[1] as f32 - 0.5
    };
    let scissor_ok = d.dynamic.scissor.is_none_or(|[sx, sy, sw, shh]| {
        holds(
            sx as f32 * scale,
            sy as f32 * scale,
            sw as f32 * scale,
            shh as f32 * scale,
        )
    });
    let i = usize::from(slot).min(7);
    vertices_ok
        && d.attributes.is_empty()
        && d.streams.is_empty()
        && holds(x, y, w, h)
        && scissor_ok
        && d.write_masks[i] & 0xf == 0xf
        && !d.blend[i].enable
        && !d.depth_stencil.depth_test
        && !d.depth_stencil.stencil_test
        && !d.raster.alpha_to_coverage
        && d.raster.polygon_mode == 2
}

pub(super) fn setup(sh: &Shared<'_>, pass: &Pass) -> Result<Setup, Skip> {
    let attachments = sh.plan.attachments_of(pass);
    let color_aliases: Vec<_> = attachments
        .iter()
        .filter_map(|a| {
            let Slot::Color(slot) = a.slot else {
                return None;
            };
            let res = sh.textures.get(&a.tex)?;
            Some((
                slot,
                (res.barrier_id, a.view.level(), a.view.layer()),
                a.load == LoadAction::Clear,
            ))
        })
        .collect();
    let has_aliases = color_aliases
        .iter()
        .enumerate()
        .any(|(i, a)| color_aliases[..i].iter().any(|b| a.1 == b.1));
    let selected = if has_aliases {
        let mut written = 0u8;
        let mut outputs = crate::draws::FastMap::default();
        for n in draw_range(sh, pass) {
            let Some(d) = sh.detail.draws.get(n as usize) else {
                continue;
            };
            let Some(key) = &d.program.fragment else {
                continue;
            };
            let locations = if let Some(mask) = outputs.get(key) {
                *mask
            } else {
                let fs = sh.programs.stage(key, false)?;
                let mask = fs.colors.as_ref().map_or(u8::MAX, |colors| {
                    colors
                        .iter()
                        .filter(|c| **c < 8)
                        .fold(0, |m, c| m | (1 << c))
                });
                outputs.insert(key, mask);
                mask
            };
            for slot in 0..8 {
                if d.write_masks[slot] != 0 && locations & (1 << slot) != 0 {
                    written |= 1 << slot;
                }
            }
        }
        match distinct_color_slots(&color_aliases, written) {
            Ok(selected) => selected,
            Err(why) => {
                if crate::diag("target") && crate::diag::sampled() {
                    eprintln!(
                        "DIAG alias conflict cmds {:?} attachments {:?}",
                        pass.cmds, color_aliases
                    );
                    for n in draw_range(sh, pass) {
                        if let Some(d) = sh.detail.draws.get(n as usize) {
                            eprintln!(
                                "DIAG alias draw {n} fs {:?} masks {:?}",
                                d.program.fragment, d.write_masks
                            );
                        }
                    }
                }
                return Err(why);
            }
        }
    } else {
        u8::MAX
    };
    let mut colors: Vec<(u8, ColorAttachment)> = Vec::new();
    let mut desc = RenderPassDesc::default();
    let mut size: Option<[u32; 2]> = None;
    let mut scale = 1.0;
    let mut has_stencil = false;
    for a in sh.plan.attachments_of(pass) {
        if matches!(a.slot, Slot::Color(n) if selected & (1 << n) == 0) {
            continue;
        }
        let res = sh.textures.get(&a.tex).ok_or(Skip::Texture)?;
        let texture = sh.texture_store.target_view(res, a.view)?;
        let d = texture.desc();
        if crate::diag("draw")
            && crate::diag::sampled()
            && crate::diag::tex_match(d.format, d.width, d.height, d.array_layers)
        {
            eprintln!(
                "DIAG attachment {:?} {:?} backing {:?} {:?} {}x{} cmds {:?}",
                a.slot, a.tex, res.barrier_id, d.format, d.width, d.height, pass.cmds
            );
        }
        // The game encodes block compressed textures on the GPU by rendering into them. Metal cannot render into a
        // compressed format, so such a pass is left out.
        if matches!(a.slot, Slot::Color(_)) && (d.format.is_compressed() || d.format.is_depth()) {
            target_diag(format_args!(
                "not renderable {:?} {}x{} cmds {:?}",
                d.format, d.width, d.height, pass.cmds
            ));
            nxkit_nvn::diag::example(
                nxkit_nvn::diag::Reason::SkipFormat,
                format_args!(
                    "colour target {:?} {:?} {}x{}x{}",
                    a.slot, d.format, d.width, d.height, d.depth
                ),
            );
            return Err(Skip::Format);
        }
        let mem = d.storage == Storage::Memoryless;
        // Attachments of different sizes render into the smallest one, as in NVN.
        let s = size.get_or_insert([d.width, d.height]);
        if *s != [d.width, d.height] {
            target_diag(format_args!(
                "size mismatch {:?} vs {}x{} slot {:?}",
                s, d.width, d.height, a.slot
            ));
            *s = [s[0].min(d.width), s[1].min(d.height)];
        }
        if res.scaled {
            scale = sh.scale;
        }
        let mut l = load(a.load, mem);
        // A colour load that only the default rule asks for goes when the draw covers the target.
        if let Slot::Color(n) = a.slot
            && a.load_is_default()
            && !mem
            && l == MLoad::Load
        {
            let covers = first_draw_covers(
                sh,
                pass,
                n,
                [d.width, d.height],
                if res.scaled { sh.scale } else { 1.0 },
            );
            if crate::diag("passmerge") && crate::diag::sampled() {
                eprintln!(
                    "DIAG passmerge cover cmds {:?} slot {n} {}x{} first draw covers {covers}",
                    pass.cmds, d.width, d.height
                );
            }
            if covers && crate::knob("coverload") {
                l = MLoad::DontCare;
            }
        }
        let s = store(a.store, mem);
        let clear = clear_of(sh, pass, a.slot);
        match a.slot {
            Slot::Color(n) => colors.push((
                n,
                ColorAttachment {
                    depth_plane: if d.kind == nxkit_metal::TextureKind::D3 {
                        a.view.layer()
                    } else {
                        0
                    },
                    texture: texture.clone(),
                    load: l,
                    store: s,
                    clear: clear.color.map(f64::from),
                    memoryless: mem,
                },
            )),
            Slot::Depth => {
                if crate::diag("depth") {
                    static SHOWN: std::sync::atomic::AtomicU32 =
                        std::sync::atomic::AtomicU32::new(0);
                    if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 500 < 20 {
                        eprintln!(
                            "DIAG depth pass {:?} {}x{} {:?} load {:?} store {:?} clear {} cmds {:?}",
                            a.tex,
                            d.width,
                            d.height,
                            d.format,
                            a.load,
                            a.store,
                            clear.depth,
                            pass.cmds
                        );
                    }
                }
                desc.depth = Some(DepthAttachment {
                    texture: texture.clone(),
                    load: l,
                    store: s,
                    clear: f64::from(clear.depth),
                    memoryless: mem,
                });
                if d.format.has_stencil() {
                    has_stencil = true;
                    desc.stencil = Some(StencilAttachment {
                        texture: texture.clone(),
                        load: l,
                        store: s,
                        clear: clear.stencil,
                        memoryless: mem,
                    });
                }
            }
        }
    }
    colors.sort_by_key(|(n, _)| *n);
    if colors.windows(2).any(|w| w[0].0 == w[1].0) {
        target_diag(format_args!(
            "twice {:?}",
            colors.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        ));
        return Err(Skip::Target);
    }
    if size.is_none() {
        target_diag(format_args!("none, cmds {:?}", pass.cmds));
    }
    let size = size.ok_or(Skip::Target)?;
    // NVN allows empty colour slots between attachments. They stay empty in the pass and in the pipeline.
    let slots = colors.last().map_or(0, |(n, _)| usize::from(*n) + 1);
    let mut formats = vec![None; slots];
    desc.colors = vec![None; slots];
    for (n, c) in colors {
        formats[usize::from(n)] = Some(c.texture.desc().format);
        desc.colors[usize::from(n)] = Some(c);
    }
    let pc = PassCtx {
        targets: Targets {
            colors: formats,
            samples: 1,
        },
        size,
        scale,
        has_depth: desc.depth.is_some(),
        has_stencil,
    };
    Ok(Setup { desc, pc })
}

/// NVN may bind a texture to several slots and select the writer with channel masks. Metal gives each attachment
/// its own tile storage, so storing an inactive alias can overwrite the active output. Preserve the slot of the
/// actual writer (or clear), leaving the other aliases empty rather than compacting the output locations.
pub(super) fn distinct_color_slots<K: Copy + Eq + std::fmt::Debug>(
    colors: &[(u8, K, bool)],
    written: u8,
) -> Result<u8, Skip> {
    let mut selected = 0u8;
    for (i, &(slot, backing, _)) in colors.iter().enumerate() {
        if colors[..i].iter().any(|c| c.1 == backing) {
            continue;
        }
        let aliases: Vec<_> = colors.iter().filter(|c| c.1 == backing).collect();
        let writers: Vec<_> = aliases
            .iter()
            .filter(|c| written & (1 << c.0) != 0)
            .collect();
        if writers.len() > 1 {
            if crate::diag("target") && crate::diag::sampled() {
                eprintln!("DIAG target competing aliases {backing:?} written {written:#x}");
            }
            return Err(Skip::Target);
        }
        let winner = writers
            .first()
            .map(|c| c.0)
            .or_else(|| aliases.iter().find(|c| c.2).map(|c| c.0))
            .unwrap_or(slot);
        selected |= 1 << winner;
    }
    Ok(selected)
}
