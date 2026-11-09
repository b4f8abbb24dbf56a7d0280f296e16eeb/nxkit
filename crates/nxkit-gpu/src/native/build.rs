//! Making the plan of a run ready: the bindings it uses, the barriers between its steps, its buffers and pipelines.

use super::*;

pub(super) fn used_resources(plan: &Plan) -> Vec<GameResource> {
    let mut out: Vec<GameResource> = Vec::new();
    for b in plan
        .steps
        .iter()
        .flat_map(|s| s.buffers.iter().chain(&s.textures))
    {
        if let Source::Game(g) = b.source
            && !out.contains(&g)
        {
            out.push(g);
        }
    }
    out
}

pub(super) fn uses_resource(plan: &Plan, r: GameResource) -> bool {
    plan.steps
        .iter()
        .flat_map(|s| s.buffers.iter().chain(&s.textures))
        .any(|b| b.source == Source::Game(r))
}

/// The barrier flags of a plan: step `i` gets a barrier before it when it reads or writes something that a step since
/// the last barrier wrote, or writes something that such a step read.
pub(super) fn barrier_flags(steps: &[Step]) -> Vec<bool> {
    let (mut written, mut read): (Vec<Source>, Vec<Source>) = (Vec::new(), Vec::new());
    let mut flags = Vec::with_capacity(steps.len());
    for s in steps {
        let all: Vec<&Binding> = s.buffers.iter().chain(&s.textures).collect();
        let hazard = all
            .iter()
            .any(|b| written.contains(&b.source) || (b.writes && read.contains(&b.source)));
        if hazard {
            written.clear();
            read.clear();
        }
        flags.push(hazard);
        for b in all {
            if b.writes {
                written.push(b.source);
            } else {
                read.push(b.source);
            }
        }
    }
    flags
}

/// The game resources the plan writes.
pub(super) fn written_resources(plan: &Plan) -> Vec<GameResource> {
    let mut out = Vec::new();
    for b in plan
        .steps
        .iter()
        .flat_map(|s| s.buffers.iter().chain(&s.textures))
        .filter(|b| b.writes)
    {
        if let Source::Game(g) = b.source
            && !out.contains(&g)
        {
            out.push(g);
        }
    }
    out
}

/// Makes the plan's buffers and pipelines.
pub(super) fn build(
    net: &dyn Replacement,
    blobs: &[&[u8]],
    cx: &PrepareCx<'_>,
) -> Result<Entry, Refusal> {
    let name = net.name();
    if !net.accepts(blobs) {
        return refuse(
            "weights",
            format!("{name}: the bound weights are not the network's"),
        );
    }
    let plan = net
        .plan(blobs)
        .map_err(|e| Refusal("build", format!("{name}: {e}")))?;
    let mut buffers = Vec::with_capacity(plan.buffers.len());
    for b in &plan.buffers {
        let made = DataBuffer::new(cx.ctx, b.bytes).and_then(|buf| match &b.init {
            Some(init) => buf.write(0, init).map(|()| buf),
            None => Ok(buf),
        });
        buffers.push(Arc::new(made.map_err(|e| {
            Refusal("buffer", format!("{name}: buffer {}: {e}", b.name))
        })?));
    }
    let mut pipelines = Vec::with_capacity(plan.steps.len());
    for (i, s) in plan.steps.iter().enumerate() {
        if s.threadgroup_bytes != 0 {
            return refuse(
                "tgmem",
                format!(
                    "{name}: step {i} {} wants {} bytes of threadgroup memory, which the compute pass cannot set yet",
                    s.kernel, s.threadgroup_bytes
                ),
            );
        }
        let bad_buffer = s.buffers.iter().find(|b| match b.source {
            Source::Buffer(n) => n >= buffers.len(),
            Source::Game(g) => !matches!(g, GameResource::Push(_)),
        });
        let bad_texture = s.textures.iter().find(|b| match b.source {
            Source::Buffer(_) => true,
            Source::Game(g) => matches!(g, GameResource::Push(_)),
        });
        if let Some(b) = bad_buffer.or(bad_texture) {
            return refuse(
                "spec",
                format!(
                    "{name}: step {i} {} binds {:?} at {} where it cannot go",
                    s.kernel, b.source, b.index
                ),
            );
        }
        let p = cx
            .pipelines
            .host_compute(name, net.source(), s.kernel)
            .map_err(|_| {
                Refusal(
                    "pipeline",
                    format!("{name}: kernel {} did not build", s.kernel),
                )
            })?;
        pipelines.push(p);
    }
    let barrier_before = barrier_flags(&plan.steps);
    let written = written_resources(&plan);
    Ok(Entry {
        plan,
        buffers,
        pipelines,
        barrier_before,
        written,
    })
}
