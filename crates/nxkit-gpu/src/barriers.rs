//! Barriers only where data flows (R19).
//!
//! Metal 4 runs the passes of a queue without order unless a barrier says otherwise. The planner walks the passes in
//! order and remembers for each texture which stages wrote it and which stages read it since the last barrier that
//! covered them. A pass gets one barrier before its first command when it reads a texture that an earlier pass wrote,
//! or writes a texture that an earlier pass wrote or read. The barrier names the stages that did the earlier work and
//! the stages of the pass. All other pass boundaries get no barrier and count as avoided.
//!
//! The planner follows textures and buffers. Buffers are tracked by range of guest address (`hazards.rs`): the storage
//! buffers that dispatches and shaders write, the buffers that copies write, and the uniform, vertex, index and
//! indirect buffers that passes read. Each access names the stages of the pass that make it: a texture that only a
//! fragment shader samples is read in the fragment stage, an attachment is written in the fragment stage, a dispatch
//! works in the dispatch stage, a copy in the blit stage. A barrier waits for the stages that did the earlier work
//! (`after`) and holds the stages that need the data (`before`) and no more.
//!
//! A barrier orders the work of its own pass only. Pass `m` can leave out the stages of an earlier writer `j` from its
//! `after` set when another stage that it waits for already waited for `j` (an earlier pass `k` that has a barrier
//! after `j`'s stage, and `m` waits for `k`'s stage). The planner keeps `order[s][a]` for that: the newest point `W`
//! such that a stage `s` of some earlier pass starts after all passes before `W` finished their stage `a`.
//! `oldbarriers` selects the planner of before: textures only, and a hazard is forgotten once any barrier covered it.
//!
//! A barrier after queue stages also covers the passes of earlier command buffers on the same queue. This is assumed
//! from the Metal 4 documentation and is not verified on hardware.

use crate::draws::FastMap;
use crate::hazards::{BufUse, Last, NO, PassUse, Ranges};

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

/// Records that pass `at` used a resource in `stages`.
fn stamp(last: &mut Last, stages: Stages, at: i64) {
    for (l, st) in last.iter_mut().zip(Stages::EACH) {
        if stages.contains(st) {
            *l = (*l).max(at);
        }
    }
}

/// One thing that a pass has to wait for: stage `a` of the pass numbered `j` touched what this pass touches in the
/// stages `c`.
#[derive(Clone, Copy)]
struct Haz {
    j: i64,
    a: Stages,
    c: Stages,
    /// The earlier work wrote, so the barrier has to make the writes visible.
    data: bool,
    /// A texture (else a buffer).
    tex: bool,
    /// This pass writes what the earlier pass only read.
    war: bool,
}

impl Haz {
    fn new(j: i64, stage: usize, c: Stages, data: bool, tex: bool, war: bool) -> Self {
        Self { j, a: Stages::EACH[stage], c, data, tex, war }
    }
}

/// Adds a hazard, or merges it into the one with the same pass, stage and resource kind.
fn push_hazard(v: &mut Vec<Haz>, h: Haz) {
    match v.iter_mut().find(|x| x.j == h.j && x.a == h.a && x.tex == h.tex) {
        Some(x) => {
            x.c = x.c | h.c;
            x.data |= h.data;
            x.war |= h.war;
        }
        None => v.push(h),
    }
}

/// The textures that a pass touches, with the stages and whether it writes them. Aliases are resolved.
fn accesses(
    frame: &Frame,
    plan: &PassPlan,
    p: &Pass,
    mine: Stages,
    uses: Option<&PassUse>,
    aliases: &FastMap<TexId, TexId>,
) -> Vec<(TexId, Stages, bool)> {
    let t = touch(frame, plan, p);
    let render = p.kind == PassKind::Render;
    let mut v: Vec<(TexId, Stages, bool)> = Vec::with_capacity(t.reads.len() + t.writes.len());
    let mut add = |id: &TexId, stages: Stages, write: bool| {
        let id = *aliases.get(id).unwrap_or(id);
        match v.iter_mut().find(|x| x.0 == id) {
            Some(x) => {
                x.1 = x.1 | stages;
                x.2 |= write;
            }
            None => v.push((id, stages, write)),
        }
    };
    for id in &t.reads {
        let stages = if !render {
            mine
        } else if t.sampled.contains(id) {
            // A sampled texture is read in the stages whose shaders sample it, all of them when unknown.
            uses.and_then(|u| u.tex.iter().find(|(x, _)| x == id))
                .map_or(Stages::VERTEX | Stages::FRAGMENT, |(_, s)| *s)
        } else {
            // An attachment that the pass loads.
            Stages::FRAGMENT
        };
        add(id, stages, false);
    }
    for id in &t.writes {
        add(id, if render { Stages::FRAGMENT } else { mine }, true);
    }
    v
}

/// Compares the barriers in force with what the hazards need.
fn compare(before: &[Option<Dependency>], needed: &[Option<(Stages, Stages)>], mut r: Report) -> Report {
    for (e, n) in before.iter().zip(needed) {
        match (e, n) {
            (Some(d), Some((after, wait))) => {
                r.over_after += u32::from(!after.contains(d.after));
                r.over_before += u32::from(!wait.contains(d.before));
            }
            (Some(_), None) => r.no_hazard += 1,
            (None, Some(_)) => r.missed += 1,
            (None, None) => {}
        }
    }
    r
}

impl Report {
    fn add(&mut self, o: &Report) {
        self.needed += o.needed;
        self.tex_raw += o.tex_raw;
        self.tex_war += o.tex_war;
        self.buf_raw += o.buf_raw;
        self.buf_war += o.buf_war;
        self.covered += o.covered;
    }
}

impl BarrierPlan {
    /// The lines of `NXKIT_DIAG=barr` for the plan.
    pub(crate) fn describe(&self) -> [String; 2] {
        let d = self.distance;
        let r = self.report;
        [
            format!(
                "DIAG barr distance to the work waited for: earlier frame {} | 1 {} | 2 {} | 3-5 {} | more {} (of {} barriers, {})",
                d[0],
                d[1],
                d[2],
                d[3],
                d[4],
                self.emitted,
                if crate::knob("oldbarriers") { "oldbarriers" } else { "hazard plan" },
            ),
            format!(
                "DIAG barr hazards: {} passes depend on earlier work | texture write then read/write {}, texture read then write {}, \
                 buffer write then read/write {}, buffer read then write {} | in force: no hazard {}, hold stages the pass does not \
                 use {}, wait for stages that did nothing of it {}, missing {} | hazards covered by an earlier barrier {}",
                r.needed,
                r.tex_raw,
                r.tex_war,
                r.buf_raw,
                r.buf_war,
                r.no_hazard,
                r.over_before,
                r.over_after,
                r.missed,
                r.covered,
            ),
        ]
    }
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
        let barriers = state.plan(&frame, &plan, &aliases, &[], &[]);
        assert_eq!(barriers.emitted, 1);
        let dep = barriers.before[0].unwrap();
        assert_eq!(dep.after, Stages::FRAGMENT);
        assert_eq!(dep.before, Stages::DISPATCH);
        assert_eq!(dep.visibility, Visibility::Device);
    }

    #[test]
    fn a_render_pass_holds_only_the_stages_that_sample() {
        use crate::hazards::PassUse;
        let mut rec = nxkit_nvn::frame::CmdStream::new();
        let mut rt = nxkit_nvn::frame::RtSet::default();
        rt.color[0] = Some(TexId(1));
        rec.cmds.push(Cmd::SetRenderTargets(rt));
        let reads = rec.push_list(&[TexId(0)]);
        rec.cmds.push(Cmd::Draw { state: nxkit_nvn::frame::DrawState { color_write: 1, ..Default::default() }, reads });
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
            table.create(desc, nxkit_nvn::space::GpuAddr::new(0x10_0000 + i * 0x1000).unwrap()).unwrap();
        }
        let mut plan = PassPlan::default();
        Analyser::new().analyse(&frame, &table, &mut plan).unwrap();
        let render = plan.passes.iter().position(|p| p.kind == PassKind::Render).unwrap();
        for (read_in, want) in [
            (Stages::FRAGMENT, Stages::FRAGMENT),
            (Stages::VERTEX, Stages::VERTEX),
            (Stages::VERTEX | Stages::FRAGMENT, Stages::VERTEX | Stages::FRAGMENT),
        ] {
            let mut state = BarrierState::default();
            state.seed(TexId(0), Stages::FRAGMENT);
            let mut uses: Vec<PassUse> = plan.passes.iter().map(|_| PassUse::default()).collect();
            uses[render].tex.push((TexId(0), read_in));
            let barriers = state.plan(&frame, &plan, &FastMap::default(), &[], &uses);
            let dep = barriers.before[render].unwrap();
            assert_eq!(dep.before, want);
            assert_eq!(dep.after, Stages::FRAGMENT);
        }
    }
}

/// The barrier that a render pass with an indirect or mesh draw starts with, besides the one of the planner.
///
/// The pre-pass of such a pass copies the draw arguments in a compute pass that waits for all earlier passes, so the
/// pass only waits for that dispatch (`prepass`). Before, it also waited for all earlier dispatches, copies and
/// fragment work (`oldbarriers`): the planner follows textures only there. The storage, index and indirect buffers
/// that the pass reads are buffer hazards of the planner now.
pub(crate) fn gpu_driven(prepass: bool) -> Option<Dependency> {
    let old = crate::knob("oldbarriers");
    if !old && !prepass {
        return None;
    }
    Some(Dependency {
        scope: Scope::FromEarlierPasses,
        after: if old { Stages::DISPATCH | Stages::BLIT | Stages::FRAGMENT } else { Stages::DISPATCH },
        before: Stages::VERTEX | Stages::OBJECT | Stages::MESH,
        visibility: Visibility::Device,
    })
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
    /// The state of `oldbarriers`.
    pending: FastMap<TexId, Hazard>,
    /// The last pass per stage that wrote or read each texture, as global pass numbers.
    tex: FastMap<TexId, TexLast>,
    /// The ranges that passes wrote, and the ranges that they read.
    written: Ranges,
    read: Ranges,
    /// `order[s][a]`: stage `s` of some earlier pass starts after every pass before this global number finished its
    /// stage `a`.
    order: [[i64; 6]; 6],
    /// The global number of the first pass of the next frame. Numbers grow over frames: the queue is one.
    base: i64,
}

/// The last pass per stage that wrote and read a texture.
#[derive(Clone, Copy)]
struct TexLast {
    w: Last,
    r: Last,
}

impl Default for TexLast {
    fn default() -> Self {
        Self { w: [NO; 6], r: [NO; 6] }
    }
}

/// What the hazard analysis says about the barriers of a frame, for `NXKIT_DIAG=barr`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Report {
    /// Passes that really depend on earlier work.
    pub(crate) needed: u32,
    /// Of those, with a hazard on a texture that was written (read after write, write after write).
    pub(crate) tex_raw: u32,
    /// With a write after a read of a texture.
    pub(crate) tex_war: u32,
    /// With a hazard on a buffer that was written.
    pub(crate) buf_raw: u32,
    /// With a write after a read of a buffer.
    pub(crate) buf_war: u32,
    /// Barriers that were emitted with no real hazard behind them.
    pub(crate) no_hazard: u32,
    /// Barriers that hold stages of the pass that touch nothing of the hazards.
    pub(crate) over_before: u32,
    /// Barriers that wait for stages of earlier work that wrote or read nothing of the hazards.
    pub(crate) over_after: u32,
    /// Hazards that an earlier barrier already covered, so they added no `after` stage.
    pub(crate) covered: u32,
    /// Real hazards that no barrier was emitted for (the plan in force does not know them).
    pub(crate) missed: u32,
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
    /// With `NXKIT_DIAG=barr`: the hazard analysis compared with the barriers in force.
    pub(crate) report: Report,
}

impl BarrierState {
    /// Notes a texture that work before the frame wrote (an upload in the first command buffer).
    pub(crate) fn seed(&mut self, tex: TexId, stages: Stages) {
        let h = self.pending.entry(tex).or_default();
        h.write = join(h.write, stages);
        let t = self.tex.entry(tex).or_default();
        stamp(&mut t.w, stages, self.base - 1);
    }

    /// Notes a texture that work after the passes of the frame read (a copy-back at the end of the last command
    /// buffer), so the next pass that writes it waits for that read.
    pub(crate) fn note_read(&mut self, tex: TexId, stages: Stages) {
        let h = self.pending.entry(tex).or_default();
        h.read = join(h.read, stages);
        let t = self.tex.entry(tex).or_default();
        stamp(&mut t.r, stages, self.base - 1);
    }

    /// Forgets a texture (the game finalized it).
    pub(crate) fn forget(&mut self, tex: TexId) {
        self.pending.remove(&tex);
        self.tex.remove(&tex);
    }

    /// Plans the barriers of `plan` and updates the state. `uses` is the output of `hazards::pass_uses`, one entry
    /// per pass (or empty, then every sampled texture counts as read in all stages of its pass and buffers are not
    /// followed). `vertex_samples` is for `oldbarriers`.
    pub(crate) fn plan(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        aliases: &FastMap<TexId, TexId>,
        vertex_samples: &[bool],
        uses: &[PassUse],
    ) -> BarrierPlan {
        // Both planners always run, so the state of either is right when `oldbarriers` is switched.
        let (mut new, needed) = self.plan_hazards(frame, plan, aliases, uses);
        let mut old = self.plan_old(frame, plan, aliases, vertex_samples);
        if crate::knob("oldbarriers") {
            let kinds = new.report;
            old.report = compare(&old.before, &needed, kinds);
            old
        } else {
            new.report = compare(&new.before, &needed, new.report);
            new
        }
    }

    /// The planner of before the hazard analysis (`oldbarriers`).
    fn plan_old(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        aliases: &FastMap<TexId, TexId>,
        vertex_samples: &[bool],
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
            let t = touch(frame, plan, p);
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
            }
            for id in &t.writes {
                let id = aliases.get(id).unwrap_or(id);
                let h = self.pending.entry(*id).or_default();
                h.last = Some(index);
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
                h.last = Some(index);
                h.read = join(h.read, mine);
            }
            out.before.push(dep);
        }
        out
    }

    /// The planner of the hazard analysis. Gives the plan and, per pass, the `(after, before)` stages that the hazards
    /// need, before the knobs `allbar`, `nobar` and `nobarrier` change the barrier.
    fn plan_hazards(
        &mut self,
        frame: &Frame,
        plan: &PassPlan,
        aliases: &FastMap<TexId, TexId>,
        uses: &[PassUse],
    ) -> (BarrierPlan, Vec<Option<(Stages, Stages)>>) {
        let mut out = BarrierPlan::default();
        let mut needed = Vec::with_capacity(plan.passes.len());
        let mut first = true;
        for (index, p) in plan.passes.iter().enumerate() {
            if !matches!(p.kind, PassKind::Render | PassKind::Compute | PassKind::Copy) {
                out.before.push(None);
                needed.push(None);
                continue;
            }
            let g = self.base + index as i64;
            let mine = pass_stages(p.kind);
            let pu = uses.get(index);
            let acc = accesses(frame, plan, p, mine, pu, aliases);
            let bufs: &[BufUse] = pu.map_or(&[], |u| &u.bufs);
            let mut hz: Vec<Haz> = Vec::new();
            for (id, c, write) in &acc {
                let Some(t) = self.tex.get(id) else { continue };
                for (ix, &j) in t.w.iter().enumerate() {
                    if j != NO {
                        push_hazard(&mut hz, Haz::new(j, ix, *c, true, true, false));
                    }
                }
                if *write {
                    for (ix, &j) in t.r.iter().enumerate() {
                        if j != NO {
                            push_hazard(&mut hz, Haz::new(j, ix, *c, false, true, true));
                        }
                    }
                }
            }
            for u in bufs {
                for (_, _, l) in self.written.overlapping(u.lo, u.hi) {
                    for (ix, &j) in l.iter().enumerate() {
                        if j != NO {
                            push_hazard(&mut hz, Haz::new(j, ix, u.stages, true, false, false));
                        }
                    }
                }
                if u.write {
                    for (_, _, l) in self.read.overlapping(u.lo, u.hi) {
                        for (ix, &j) in l.iter().enumerate() {
                            if j != NO {
                                push_hazard(&mut hz, Haz::new(j, ix, u.stages, false, false, true));
                            }
                        }
                    }
                }
            }
            // The stages that wait: those of the pass that touch what was touched. The stages waited for: the
            // smallest set that covers every hazard directly or through an earlier barrier.
            let before = hz.iter().fold(Stages::NONE, |a, h| a | h.c);
            let covered = |h: &Haz, after: Stages| {
                after.contains(h.a)
                    || Stages::EACH
                        .iter()
                        .enumerate()
                        .any(|(s, st)| after.contains(*st) && self.order[s][h.a.index()] > h.j)
            };
            let mut after = Stages::NONE;
            for h in &hz {
                if !covered(h, after) {
                    after = after | h.a;
                }
            }
            for st in Stages::EACH {
                let less = after.minus(st);
                if after.contains(st) && hz.iter().all(|h| covered(h, less)) {
                    after = less;
                }
            }
            let mut report = Report::default();
            let dep = (!hz.is_empty()).then(|| {
                report.needed = 1;
                report.tex_raw = u32::from(hz.iter().any(|h| h.tex && h.data));
                report.tex_war = u32::from(hz.iter().any(|h| h.tex && h.war));
                report.buf_raw = u32::from(hz.iter().any(|h| !h.tex && h.data));
                report.buf_war = u32::from(hz.iter().any(|h| !h.tex && h.war));
                report.covered = hz.iter().filter(|h| !after.contains(h.a)).count() as u32;
                Dependency {
                    scope: Scope::FromEarlierPasses,
                    after,
                    before,
                    visibility: if hz.iter().any(|h| h.data) {
                        Visibility::Device
                    } else {
                        Visibility::None
                    },
                }
            });
            needed.push(dep.map(|d| (d.after, d.before)));
            out.report.add(&report);
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
                let nearest = hz.iter().map(|h| h.j - self.base).max();
                let d = nearest.filter(|n| *n >= 0).map_or(0, |n| index as i64 - n);
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
                self.note_barrier(g, d);
            }
            for (id, c, write) in acc {
                let t = self.tex.entry(id).or_default();
                stamp(if write { &mut t.w } else { &mut t.r }, c, g);
            }
            for u in bufs {
                let set = if u.write { &mut self.written } else { &mut self.read };
                set.add(u.lo, u.hi, u.stages, g);
            }
            out.before.push(dep);
        }
        self.base += plan.passes.len() as i64;
        (out, needed)
    }

    /// Notes what a barrier at global pass `g` orders: its `before` stages start after every earlier pass finished
    /// the `after` stages, and after what those passes themselves waited for.
    fn note_barrier(&mut self, g: i64, dep: &Dependency) {
        let mut reach = [NO; 6];
        for (a, r) in reach.iter_mut().enumerate() {
            if dep.after.contains(Stages::EACH[a]) {
                *r = g;
            }
            for (s, st) in Stages::EACH.iter().enumerate() {
                if dep.after.contains(*st) {
                    *r = (*r).max(self.order[s][a]);
                }
            }
        }
        for (s, st) in Stages::EACH.iter().enumerate() {
            if dep.before.contains(*st) {
                for (o, r) in self.order[s].iter_mut().zip(reach) {
                    *o = (*o).max(r);
                }
            }
        }
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
