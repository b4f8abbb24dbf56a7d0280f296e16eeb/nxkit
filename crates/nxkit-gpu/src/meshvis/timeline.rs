//! The time line of each depth texture that a pass with mesh draws uses: its passes, the clears under a render
//! condition and the copies that read or write it, in the order of the frame. It shows whether a clear or a pre-pass
//! on the texture comes before or after the colour pass.

use std::collections::BTreeSet;

use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{LoadAction, PassKind, PassPlan};
use nxkit_nvn::resource::TexId;

use crate::detail::{CopyKind, FrameDetail};
use crate::draws::Ordinal;

/// The most time lines of a frame.
pub(crate) const MAX_LINES: usize = 120;

/// One event on a depth texture.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Event {
    /// A render pass with the texture as its depth attachment.
    Pass {
        index: usize,
        load: String,
        clear: Option<f32>,
        store: String,
        draws: u32,
        mesh: u32,
    },
    /// A depth clear that runs under a render condition.
    Clear {
        value: f32,
        cond: String,
        decision: String,
    },
    /// A copy that reads or writes the texture. `from` is true when the texture is the destination and `other` is
    /// the source.
    Copy {
        from: bool,
        other: Option<TexId>,
        kind: String,
        size: [u32; 3],
        refused: bool,
        skipped: bool,
    },
}

/// An event with the command it belongs to and the texture.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Timed {
    pub(crate) at: u32,
    pub(crate) tex: TexId,
    pub(crate) ev: Event,
}

impl Event {
    fn text(&self) -> String {
        let id = |t: &Option<TexId>| t.map_or_else(|| "-".to_owned(), |t| format!("{t:?}"));
        match self {
            Self::Pass {
                index,
                load,
                clear,
                store,
                draws,
                mesh,
            } => format!(
                "pass {index} load {load} clear {} store {store} draws {draws} mesh {mesh}",
                clear.map_or_else(|| "-".to_owned(), |c| c.to_string())
            ),
            Self::Clear {
                value,
                cond,
                decision,
            } => format!("clear {value} cond {cond} decision {decision}"),
            Self::Copy {
                from,
                other,
                kind,
                size,
                refused,
                skipped,
            } => format!(
                "copy {} {} kind {kind} size {}x{}x{}{}{}",
                if *from { "from" } else { "to" },
                id(other),
                size[0],
                size[1],
                size[2],
                if *refused { " refused" } else { "" },
                if *skipped { " skipped" } else { "" }
            ),
        }
    }
}

/// The lines of the time lines, one block for each texture in the order of its first event, the events of a block in
/// the order of the frame. At most [`MAX_LINES`], then one line with the number left out.
pub(crate) fn time_lines(frame: u64, events: &[Timed]) -> Vec<String> {
    let mut texes: Vec<TexId> = Vec::new();
    for e in events {
        if !texes.contains(&e.tex) {
            texes.push(e.tex);
        }
    }
    let mut out = Vec::new();
    let mut left = 0usize;
    for t in texes {
        let mut mine: Vec<&Timed> = events.iter().filter(|e| e.tex == t).collect();
        mine.sort_by_key(|e| e.at);
        for e in mine {
            if out.len() >= MAX_LINES {
                left += 1;
            } else {
                out.push(format!(
                    "DIAG meshvis frame {frame} depth {t:?}: {}",
                    e.ev.text()
                ));
            }
        }
    }
    if left > 0 {
        out.push(format!(
            "DIAG meshvis frame {frame}: {left} more time lines not listed"
        ));
    }
    out
}

/// The events of the depth textures of the passes with mesh draws. `mesh_of` gives the mesh draws of a pass.
pub(crate) fn collect(
    frame: &Frame,
    passes: &PassPlan,
    detail: &FrameDetail,
    ords: &[Ordinal],
    mesh_of: &dyn Fn(usize) -> u32,
) -> Vec<Timed> {
    let depth_of = |pi: usize| {
        let p = &passes.passes[pi];
        passes
            .attachments_of(p)
            .iter()
            .find(|a| matches!(a.slot, nxkit_nvn::passes::Slot::Depth))
            .map(|a| (a.tex, a.load, a.store))
    };
    let render = |pi: usize| passes.passes[pi].kind == PassKind::Render;
    let wanted: BTreeSet<TexId> = (0..passes.passes.len())
        .filter(|pi| render(*pi) && mesh_of(*pi) > 0)
        .filter_map(|pi| depth_of(pi).map(|d| d.0))
        .collect();
    let mut out = Vec::new();
    for pi in (0..passes.passes.len()).filter(|pi| render(*pi)) {
        let Some((tex, load, store)) = depth_of(pi).filter(|d| wanted.contains(&d.0)) else {
            continue;
        };
        let p = &passes.passes[pi];
        let draws = p
            .cmds
            .clone()
            .filter(|i| matches!(frame.cmds.get(*i as usize), Some(Cmd::Draw { .. })))
            .count() as u32;
        let clear_at = p.cmds.clone().find(|i| {
            matches!(
                frame.cmds.get(*i as usize),
                Some(Cmd::ClearDepthStencil { full: true })
            )
        });
        let value = |i: u32| {
            ords.get(i as usize)
                .and_then(|o| detail.clears.get(o.clear as usize))
                .map(|c| c.depth)
        };
        out.push(Timed {
            at: p.cmds.start,
            tex,
            ev: Event::Pass {
                index: pi,
                load: format!("{load:?}"),
                clear: clear_at
                    .filter(|_| load == LoadAction::Clear)
                    .and_then(value),
                store: format!("{store:?}"),
                draws,
                mesh: mesh_of(pi),
            },
        });
        for i in p.cmds.clone() {
            if !matches!(
                frame.cmds.get(i as usize),
                Some(Cmd::ClearDepthStencil { .. })
            ) {
                continue;
            }
            if let Some(seg) = frame.gates.iter().find(|s| s.range.contains(&i)) {
                out.push(Timed {
                    at: i,
                    tex,
                    ev: Event::Clear {
                        value: value(i).unwrap_or_default(),
                        cond: seg.info.source.name().to_owned(),
                        decision: seg.info.decision_name(),
                    },
                });
            }
        }
    }
    for (i, c) in frame.cmds.iter().enumerate() {
        let (src, dst) = match c {
            Cmd::Copy { src, dst } => (*src, *dst),
            Cmd::Downsample { src, dst } => (Some(*src), Some(*dst)),
            _ => continue,
        };
        let i = i as u32;
        let d = ords
            .get(i as usize)
            .and_then(|o| detail.copies.get(o.copy as usize))
            .copied()
            .unwrap_or_default();
        let ev = |from: bool, other: Option<TexId>| Event::Copy {
            from,
            other,
            kind: format!("{:?}", d.kind),
            size: d.size,
            refused: d.kind == CopyKind::TextureToTexture && d.size != d.dst_size,
            skipped: frame.skips(i),
        };
        if let Some(t) = dst.filter(|t| wanted.contains(t)) {
            out.push(Timed {
                at: i,
                tex: t,
                ev: ev(true, src),
            });
        }
        if let Some(t) = src.filter(|t| wanted.contains(t)) {
            out.push(Timed {
                at: i,
                tex: t,
                ev: ev(false, dst),
            });
        }
    }
    out
}
