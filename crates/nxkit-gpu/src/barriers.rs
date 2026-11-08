//! Barriers only where data flows (R19).
//!
//! Metal 4 runs the passes of a queue without order unless a barrier says otherwise. The planner walks the passes in
//! order and remembers for each texture which stages wrote it and which stages read it since the last barrier that
//! covered them. A pass gets one barrier before its first command when it reads a texture that an earlier pass wrote,
//! or writes a texture that an earlier pass wrote or read. The barrier names the stages that did the earlier work and
//! the stages of the pass. All other pass boundaries get no barrier and count as avoided.
//!
//! A barrier after queue stages also covers the passes of earlier command buffers on the same queue. This is assumed
//! from the Metal 4 documentation and is not verified on hardware.

use crate::draws::FastMap;

use nxkit_metal::{Dependency, Scope, Stages, Visibility};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{LoadAction, Pass, PassKind, PassPlan};
use nxkit_nvn::resource::TexId;

/// The textures a pass reads and writes, derived from its commands.
#[derive(Debug, Default)]
pub(crate) struct Touch {
    /// Textures that the pass samples or copies from, and attachments that it loads.
    pub(crate) reads: Vec<TexId>,
    /// Textures that the pass sampled in a shader (a subset of `reads`).
    pub(crate) sampled: Vec<TexId>,
    /// Textures that the pass writes.
    pub(crate) writes: Vec<TexId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::{analyse::Analyser, frame::Span, resource::ResourceTable};

    #[test]
    fn sampling_an_alias_waits_for_the_original_storage_write() {
        let frame = Frame {
            cmds: vec![Cmd::Dispatch {
                reads: Span { start: 0, len: 1 },
                writes: Span::default(),
            }],
            lists: vec![TexId(2)],
        };
        let mut plan = PassPlan::default();
        let mut table = ResourceTable::new();
        let desc = nxkit_nvn::layout::TextureDesc {
            target: nxkit_nvn::layout::Target::D2,
            format: 0x25,
            flags: 0,
            samples: 0,
            width: 1,
            height: 1,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        };
        for i in 0..3 {
            table
                .create(
                    desc,
                    nxkit_nvn::space::GpuAddr::new(0x2000 + i * 4096).unwrap(),
                )
                .unwrap();
        }
        Analyser::new().analyse(&frame, &table, &mut plan).unwrap();
        let mut state = BarrierState::default();
        state.seed(TexId(1), Stages::FRAGMENT);
        let aliases: FastMap<TexId, TexId> = [(TexId(2), TexId(1))].into_iter().collect();
        let barriers = state.plan(&frame, &plan, &aliases);
        assert_eq!(barriers.emitted, 1);
        let dep = barriers.before[0].unwrap();
        assert_eq!(dep.after, Stages::FRAGMENT);
        assert_eq!(dep.before, Stages::DISPATCH);
        assert_eq!(dep.visibility, Visibility::Device);
    }
}

/// The stages that a pass of a kind runs.
pub(crate) fn pass_stages(kind: PassKind) -> Stages {
    match kind {
        PassKind::Render => Stages::VERTEX | Stages::FRAGMENT,
        PassKind::Compute => Stages::DISPATCH,
        _ => Stages::BLIT,
    }
}

/// What a pass reads and writes.
pub(crate) fn touch(frame: &Frame, plan: &PassPlan, p: &Pass) -> Touch {
    let mut t = Touch { sampled: plan.reads_of(p).to_vec(), ..Touch::default() };
    t.reads.extend_from_slice(&t.sampled);
    for a in plan.attachments_of(p) {
        t.writes.push(a.tex);
        if a.load == LoadAction::Load {
            t.reads.push(a.tex);
        }
    }
    for i in p.cmds.clone() {
        match frame.cmds.get(i as usize) {
            Some(Cmd::Dispatch { writes, .. }) => t.writes.extend_from_slice(frame.list(*writes)),
            Some(Cmd::Copy { src, dst }) => {
                t.reads.extend(src);
                t.writes.extend(dst);
            }
            Some(Cmd::Downsample { dst, .. }) => t.writes.push(*dst),
            _ => {}
        }
    }
    t
}

#[derive(Clone, Copy, Default)]
struct Hazard {
    write: Option<Stages>,
    read: Option<Stages>,
}

fn join(a: Option<Stages>, b: Stages) -> Option<Stages> {
    Some(a.map_or(b, |a| a | b))
}

/// The barriers of a frame, and the state that carries to the next one.
#[derive(Default)]
pub(crate) struct BarrierState {
    pending: FastMap<TexId, Hazard>,
}

/// The barrier before each pass and the counts.
#[derive(Debug, Default)]
pub(crate) struct BarrierPlan {
    /// The barrier before pass `i`, or `None`.
    pub(crate) before: Vec<Option<Dependency>>,
    /// Barriers emitted.
    pub(crate) emitted: u32,
    /// Boundaries between encoded passes without a barrier.
    pub(crate) avoided: u32,
}

impl BarrierState {
    /// Notes a texture that work before the frame wrote (an upload in the first command buffer).
    pub(crate) fn seed(&mut self, tex: TexId, stages: Stages) {
        let h = self.pending.entry(tex).or_default();
        h.write = join(h.write, stages);
    }

    /// Forgets a texture (the game finalized it).
    pub(crate) fn forget(&mut self, tex: TexId) {
        self.pending.remove(&tex);
    }

    /// Plans the barriers of `plan` and updates the state.
    pub(crate) fn plan(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        aliases: &FastMap<TexId, TexId>,
    ) -> BarrierPlan {
        let mut out = BarrierPlan::default();
        let mut first = true;
        for p in &plan.passes {
            if !matches!(p.kind, PassKind::Render | PassKind::Compute | PassKind::Copy) {
                out.before.push(None);
                continue;
            }
            let t = touch(frame, plan, p);
            let mine = pass_stages(p.kind);
            let mut after: Option<Stages> = None;
            let mut before = if p.kind == PassKind::Render {
                Stages::FRAGMENT
            } else {
                mine
            };
            let mut writes_involved = false;
            for id in &t.reads {
                let sampled = t.sampled.contains(id);
                let id = aliases.get(id).unwrap_or(id);
                if let Some(w) = self.pending.get(id).and_then(|h| h.write) {
                    after = join(after, w);
                    writes_involved = true;
                    if sampled {
                        before = before | mine;
                    }
                }
            }
            for id in &t.writes {
                let id = aliases.get(id).unwrap_or(id);
                if let Some(h) = self.pending.get(id) {
                    for s in [h.write, h.read].into_iter().flatten() {
                        after = join(after, s);
                    }
                    writes_involved |= h.write.is_some();
                }
            }
            let dep = after.map(|after| Dependency {
                scope: Scope::FromEarlierPasses,
                after,
                before,
                visibility: if writes_involved {
                    Visibility::Device
                } else {
                    Visibility::None
                },
            });
            let dep = if crate::knob("allbar") || crate::nobar(out.before.len()) {
                Some(Dependency {
                    scope: Scope::FromEarlierPasses,
                    after: Stages::ALL,
                    before: Stages::ALL,
                    visibility: Visibility::Device,
                })
            } else {
                dep
            };
            match dep {
                Some(_) => out.emitted += 1,
                None if !first => out.avoided += 1,
                None => {}
            }
            first = false;
            if let Some(d) = &dep {
                self.cover(d.after);
            }
            for id in &t.writes {
                let id = aliases.get(id).unwrap_or(id);
                let h = self.pending.entry(*id).or_default();
                h.write = join(
                    h.write,
                    if p.kind == PassKind::Render {
                        Stages::FRAGMENT
                    } else {
                        mine
                    },
                );
            }
            for id in &t.reads {
                let id = aliases.get(id).unwrap_or(id);
                let h = self.pending.entry(*id).or_default();
                h.read = join(h.read, mine);
            }
            out.before.push(dep);
        }
        out
    }

    /// Drops the hazards that a barrier after `stages` covers.
    fn cover(&mut self, stages: Stages) {
        let covered = |s: Option<Stages>| s.is_none_or(|s| (s | stages) == stages);
        self.pending.retain(|_, h| !(covered(h.write) && covered(h.read)));
        for h in self.pending.values_mut() {
            if covered(h.write) {
                h.write = None;
            }
            if covered(h.read) {
                h.read = None;
            }
        }
    }
}
