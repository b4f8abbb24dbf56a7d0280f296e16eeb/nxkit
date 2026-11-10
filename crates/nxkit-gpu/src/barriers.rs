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

use crate::draws::{FastMap, FastSet};

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
            disabled: Vec::new(),
            counters: Default::default(),
            gates: Vec::new(),
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
        let barriers = state.plan(&frame, &plan, &aliases, &[], &[], &FastSet::default());
        assert_eq!(barriers.emitted, 1);
        let dep = barriers.before[0].unwrap();
        assert_eq!(dep.after, Stages::FRAGMENT);
        assert_eq!(dep.before, Stages::DISPATCH);
        assert_eq!(dep.visibility, Visibility::Device);
    }

    #[test]
    fn the_pass_after_a_buffer_command_waits_once_for_it() {
        let dispatch = Cmd::Dispatch {
            reads: Span::default(),
            writes: Span::default(),
        };
        let frame = Frame {
            cmds: vec![dispatch, Cmd::BufferCopy, dispatch, Cmd::BufferCopy],
            ..Frame::default()
        };
        let mut plan = PassPlan::default();
        Analyser::new()
            .analyse(&frame, &ResourceTable::new(), &mut plan)
            .unwrap();
        let kinds: Vec<_> = plan.passes.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            [
                PassKind::Compute,
                PassKind::Copy,
                PassKind::Compute,
                PassKind::Copy
            ]
        );
        let mut state = BarrierState::default();
        let barriers = state.plan(
            &frame,
            &plan,
            &FastMap::default(),
            &[],
            &[],
            &FastSet::default(),
        );
        assert!(barriers.before[0].is_none());
        // The dispatch after the first command waits for its blit and dispatch stages.
        let dep = barriers.before[2].unwrap();
        assert_eq!(dep.after, Stages::DISPATCH | Stages::BLIT);
        assert_eq!(dep.before, Stages::DISPATCH | Stages::BLIT);
        assert_eq!(dep.visibility, Visibility::Device);
    }

    #[test]
    fn a_render_pass_holds_its_vertex_stage_only_when_a_vertex_shader_samples() {
        let mut rec = nxkit_nvn::frame::CmdStream::new();
        let mut rt = nxkit_nvn::frame::RtSet::default();
        rt.color[0] = Some(TexId(1));
        rec.cmds.push(Cmd::SetRenderTargets(rt));
        let reads = rec.push_list(&[TexId(0)]);
        rec.cmds.push(Cmd::Draw {
            state: nxkit_nvn::frame::DrawState {
                color_write: 1,
                ..Default::default()
            },
            reads,
        });
        let mut frame = Frame::default();
        frame.append(&rec);
        let mut table = ResourceTable::new();
        let desc = nxkit_nvn::layout::TextureDesc {
            target: nxkit_nvn::layout::Target::D2,
            format: 0x25,
            flags: 0,
            samples: 0,
            width: 4,
            height: 4,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        };
        for i in 0..2 {
            table
                .create(
                    desc,
                    nxkit_nvn::space::GpuAddr::new(0x10_0000 + i * 0x1000).unwrap(),
                )
                .unwrap();
        }
        let mut plan = PassPlan::default();
        Analyser::new().analyse(&frame, &table, &mut plan).unwrap();
        let render = plan
            .passes
            .iter()
            .position(|p| p.kind == PassKind::Render)
            .unwrap();
        for (vertex, want) in [
            (false, Stages::FRAGMENT),
            (true, Stages::VERTEX | Stages::FRAGMENT),
        ] {
            let mut state = BarrierState::default();
            state.seed(TexId(0), Stages::FRAGMENT);
            let mut vs = vec![false; plan.passes.len()];
            vs[render] = vertex;
            let barriers = state.plan(
                &frame,
                &plan,
                &FastMap::default(),
                &vs,
                &[],
                &FastSet::default(),
            );
            assert_eq!(barriers.before[render].unwrap().before, want);
        }
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
    let mut t = Touch {
        sampled: plan.reads_of(p).to_vec(),
        ..Touch::default()
    };
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
    /// The index of the last pass of this frame that wrote or read the texture, for `NXKIT_DIAG=barr`.
    last: Option<usize>,
}

fn join(a: Option<Stages>, b: Stages) -> Option<Stages> {
    Some(a.map_or(b, |a| a | b))
}

/// The barriers of a frame, and the state that carries to the next one.
#[derive(Default)]
pub(crate) struct BarrierState {
    pending: FastMap<TexId, Hazard>,
    /// The stages of the buffer copies and fills that no barrier has covered yet. They write guest memory, and the
    /// planner does not know which buffers later passes read, so every later pass waits for them once.
    buffer_write: Option<Stages>,
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
    /// With `NXKIT_DIAG=barr`: the barriers by the distance in passes to the nearest pass whose work they wait for
    /// (unknown or an earlier frame, 1, 2, 3 to 5, more). Only a barrier with independent passes in between could let
    /// them overlap with finer dependencies.
    pub(crate) distance: [u32; 5],
}

impl BarrierState {
    /// Notes a texture that work before the frame wrote (an upload in the first command buffer).
    pub(crate) fn seed(&mut self, tex: TexId, stages: Stages) {
        let h = self.pending.entry(tex).or_default();
        h.write = join(h.write, stages);
    }

    /// Notes a texture that work after the passes of the frame read (a copy-back at the end of the last command
    /// buffer), so the next pass that writes it waits for that read.
    pub(crate) fn note_read(&mut self, tex: TexId, stages: Stages) {
        let h = self.pending.entry(tex).or_default();
        h.read = join(h.read, stages);
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
        vertex_samples: &[bool],
        images: &[crate::images::PassImages],
        blit_written: &FastSet<TexId>,
    ) -> BarrierPlan {
        let mut out = BarrierPlan::default();
        let mut first = true;
        for (index, p) in plan.passes.iter().enumerate() {
            if !matches!(
                p.kind,
                PassKind::Render | PassKind::Compute | PassKind::Copy
            ) {
                out.before.push(None);
                continue;
            }
            let mut t = touch(frame, plan, p);
            // Images that fragment shaders of the pass's draws bind are not in its commands' lists.
            if let Some(x) = images.get(index) {
                t.reads.extend_from_slice(&x.reads);
                t.writes.extend_from_slice(&x.writes);
                t.reads.extend_from_slice(&x.writes);
            }
            let mine = pass_stages(p.kind);
            let mut after: Option<Stages> = None;
            let mut before = if p.kind == PassKind::Render {
                Stages::FRAGMENT
            } else {
                mine
            };
            let mut writes_involved = false;
            let mut nearest: Option<usize> = None;
            let mut note = |h: &Hazard| {
                if let Some(l) = h.last {
                    nearest = Some(nearest.map_or(l, |n: usize| n.max(l)));
                }
            };
            for id in &t.reads {
                let sampled = t.sampled.contains(id);
                let id = aliases.get(id).unwrap_or(id);
                if let Some(h) = self.pending.get(id).filter(|h| h.write.is_some()) {
                    note(h);
                }
                if let Some(w) = self.pending.get(id).and_then(|h| h.write) {
                    after = join(after, w);
                    writes_involved = true;
                    // A sampled texture holds the vertex stage too only when a vertex shader of the pass samples.
                    // Holding it always kept the vertex work of every render pass from overlapping the fragment
                    // work before it.
                    let vertex = p.kind != PassKind::Render
                        || crate::knob("vsbar")
                        || vertex_samples.get(index).copied().unwrap_or(true);
                    if sampled && vertex {
                        before = before | mine;
                    }
                }
            }
            for id in &t.writes {
                let id = aliases.get(id).unwrap_or(id);
                if let Some(h) = self.pending.get(id) {
                    if h.write.is_some() || h.read.is_some() {
                        note(h);
                    }
                    for s in [h.write, h.read].into_iter().flatten() {
                        after = join(after, s);
                    }
                    writes_involved |= h.write.is_some();
                }
            }
            // A buffer copy or fill of an earlier pass: this pass waits in every stage it can read a buffer in.
            if let Some(w) = self.buffer_write {
                after = join(after, w);
                writes_involved = true;
                before = before
                    | if p.kind == PassKind::Render {
                        Stages::VERTEX | Stages::FRAGMENT | Stages::OBJECT | Stages::MESH
                    } else {
                        Stages::DISPATCH | Stages::BLIT
                    };
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
            // The `allbar` knob and the `nobar` file give the pass a full barrier: it waits for every stage of the
            // earlier work. The stages before the barrier must be ones that this kind of pass runs, Metal refuses
            // the others.
            let dep = if crate::knob("allbar") || crate::nobar(out.before.len()) {
                Some(Dependency {
                    scope: Scope::FromEarlierPasses,
                    after: Stages::ALL,
                    before: mine,
                    visibility: Visibility::Device,
                })
            } else if crate::knob("nobarrier") {
                None
            } else {
                dep
            };
            if dep.is_some() && crate::diag("barr") {
                let d = nearest.map_or(0, |n| index.saturating_sub(n));
                let bucket = match d {
                    0 => 0,
                    1 => 1,
                    2 => 2,
                    3..=5 => 3,
                    _ => 4,
                };
                out.distance[bucket] += 1;
            }
            match dep {
                Some(_) => out.emitted += 1,
                None if !first => out.avoided += 1,
                None => {}
            }
            first = false;
            if let Some(d) = &dep {
                self.cover(d.after);
                if self.buffer_write.is_some_and(|w| (d.after | w) == d.after) {
                    self.buffer_write = None;
                }
            }
            if p.cmds
                .clone()
                .any(|i| matches!(frame.cmds.get(i as usize), Some(Cmd::BufferCopy)))
            {
                self.buffer_write = join(self.buffer_write, Stages::DISPATCH | Stages::BLIT);
            }
            for id in &t.writes {
                let id = aliases.get(id).unwrap_or(id);
                let h = self.pending.entry(*id).or_default();
                h.last = Some(index);
                // A render pass writes a compressed attachment through a block shadow and a blit after the pass.
                let stages = match p.kind {
                    PassKind::Render if blit_written.contains(id) => {
                        Stages::FRAGMENT | Stages::BLIT
                    }
                    PassKind::Render => Stages::FRAGMENT,
                    _ => mine,
                };
                h.write = join(h.write, stages);
            }
            for id in &t.reads {
                let id = aliases.get(id).unwrap_or(id);
                let h = self.pending.entry(*id).or_default();
                h.last = Some(index);
                h.read = join(h.read, mine);
            }
            out.before.push(dep);
        }
        out
    }

    /// Drops the hazards that a barrier after `stages` covers.
    fn cover(&mut self, stages: Stages) {
        let covered = |s: Option<Stages>| s.is_none_or(|s| (s | stages) == stages);
        self.pending
            .retain(|_, h| !(covered(h.write) && covered(h.read)));
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
