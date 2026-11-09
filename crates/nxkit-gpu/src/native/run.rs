//! Encoding a prepared run, the compare of a sampled frame, the capture list and the health counters of
//! [`super`].

use super::*;

// ---------------------------------------------------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------------------------------------------------

/// Encodes the plan as one compute pass in `cb`. `before` is the barrier that the planner put before the pass of the
/// anchor dispatch. A failure here loses the run's picture, so it is counted.
pub(crate) fn encode(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    pass_index: usize,
    p: &Prepared,
    stats: &mut EncodeStats,
) {
    let before = sh.barriers.before.get(pass_index).copied().flatten();
    if let Err(why) = encode_pass(sh, cb, scratch, before, p) {
        count_fallback(p.name, "encode", &format!("pass {pass_index}: {why}"));
        stats.skip(Skip::Encode);
    } else {
        stats.compute_passes += 1;
    }
}

fn ctx(what: &'static str) -> impl FnOnce(nxkit_metal::MetalError) -> String {
    move |err| format!("{what}: {err}")
}

fn encode_pass(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    before: Option<Dependency>,
    p: &Prepared,
) -> Result<(), String> {
    let table = ArgumentTable::new(sh.ctx, 31, 16).map_err(ctx("argument table"))?;
    let mut push_addr: Vec<(GameResource, u64)> = Vec::with_capacity(p.pushes.len());
    for (r, bytes) in &p.pushes {
        push_addr.push((*r, scratch.push(bytes).map_err(ctx("push constants"))?));
    }
    let mut cp = cb.begin_compute_pass(&table).map_err(ctx("begin pass"))?;
    if crate::diag::gpu_labels() {
        cp.set_label(&format!("native {}", p.name));
    }
    if let Some(dep) = before {
        cp.encode(&ComputeCommand::Depend(dep))
            .map_err(ctx("depend"))?;
    }
    // The run's inputs and the game's own chain (on a compare frame) come from earlier passes.
    cp.encode(&ComputeCommand::WaitForEarlierPasses)
        .map_err(ctx("wait"))?;
    let tex_of = |r: GameResource| -> Option<Texture> {
        if let Some(c) = &p.compare
            && let Some((_, t)) = c.twins.iter().find(|(g, _)| *g == r)
        {
            return Some(t.clone());
        }
        p.textures
            .iter()
            .find(|(g, _, _, _)| *g == r)
            .map(|(_, t, _, _)| t.clone())
    };
    let entry = &p.entry;
    for (i, step) in entry.plan.steps.iter().enumerate() {
        if entry.barrier_before[i] {
            cp.encode(&ComputeCommand::Barrier)
                .map_err(ctx("barrier"))?;
        }
        cp.encode(&ComputeCommand::SetPipeline(entry.pipelines[i].clone()))
            .map_err(ctx("pipeline"))?;
        for b in &step.buffers {
            let addr = match b.source {
                Source::Buffer(n) => entry.buffers[n].gpu_address(),
                Source::Game(g) => push_addr
                    .iter()
                    .find(|(r, _)| *r == g)
                    .map(|(_, a)| *a)
                    .ok_or_else(|| format!("{g:?} has no bytes"))?,
            };
            cp.encode(&ComputeCommand::SetBuffer {
                index: b.index,
                gpu_addr: addr,
            })
            .map_err(ctx("set buffer"))?;
        }
        for b in &step.textures {
            let Source::Game(g) = b.source else {
                return Err(format!("step {i} binds a plan buffer as a texture"));
            };
            let texture = tex_of(g).ok_or_else(|| format!("{g:?} has no texture"))?;
            cp.encode(&ComputeCommand::SetTexture {
                index: b.index,
                texture,
            })
            .map_err(ctx("set texture"))?;
        }
        cp.encode(&ComputeCommand::DispatchThreadgroups {
            groups: step.groups,
            threads: step.threads,
        })
        .map_err(ctx("dispatch"))?;
    }
    if let Some(c) = &p.compare {
        // The game's chain has finished (the wait above). Copy its picture next to the plan's for the CPU.
        cp.encode(&ComputeCommand::Barrier)
            .map_err(ctx("barrier"))?;
        let region = nxkit_metal::TextureRegion::whole(0, 0, c.size);
        cp.encode(&ComputeCommand::CopyTexture {
            src: c.game_output.clone(),
            dst: c.snapshot.clone(),
            src_region: region,
            dst_level: 0,
            dst_slice: 0,
            dst_origin: [0, 0, 0],
        })
        .map_err(ctx("compare copy"))?;
    }
    cp.retain(entry.clone());
    if let Some(c) = &p.compare {
        cp.retain(c.clone());
    }
    cp.end();
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// The compare
// ---------------------------------------------------------------------------------------------------------------------

/// The textures of a compare frame: the plan writes the twins, the game's chain writes the real ones.
#[derive(Clone)]
pub(super) struct Compare {
    pub(super) twins: Vec<(GameResource, Texture)>,
    game_output: Texture,
    snapshot: Texture,
    size: [u32; 3],
}

impl Compare {
    pub(super) fn pending(&self, frame: u64) -> PendingCompare {
        let native = self
            .twins
            .iter()
            .find(|(g, _)| *g == GameResource::Output)
            .map(|(_, t)| t.clone());
        PendingCompare {
            frame,
            native,
            game: Some(self.snapshot.clone()),
        }
    }
}

/// A compare whose pictures are read some frames after the frame that made them.
#[derive(Default)]
pub(super) struct PendingCompare {
    pub(super) frame: u64,
    native: Option<Texture>,
    game: Option<Texture>,
}

impl PendingCompare {
    pub(super) fn report(self) {
        let (Some(n), Some(g)) = (self.native, self.game) else {
            return;
        };
        let d = n.desc();
        let bytes = d.width as usize * 4;
        let mut a = vec![0u8; bytes * d.height as usize];
        let mut b = a.clone();
        if n.read(bytes, &mut a).is_err() || g.read(bytes, &mut b).is_err() {
            eprintln!(
                "DIAG ndve compare frame {}: the pictures could not be read",
                self.frame
            );
            return;
        }
        let diff = picture_diff(&a, &b);
        eprintln!(
            "DIAG ndve compare frame {} {}x{} {:?}: max difference per channel (r,g,b,a) {:?}, differing pixels {} of {}",
            self.frame, d.width, d.height, d.format, diff.max, diff.pixels, diff.total
        );
    }
}

/// The difference of two pictures of 4 bytes per pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Diff {
    max: [u8; 4],
    pixels: u64,
    total: u64,
}

fn picture_diff(a: &[u8], b: &[u8]) -> Diff {
    let mut d = Diff {
        max: [0; 4],
        pixels: 0,
        total: (a.len() / 4) as u64,
    };
    let (pa, _) = a.as_chunks::<4>();
    let (pb, _) = b.as_chunks::<4>();
    for (x, y) in pa.iter().zip(pb) {
        let mut any = false;
        for c in 0..4 {
            let v = x[c].abs_diff(y[c]);
            d.max[c] = d.max[c].max(v);
            any |= v != 0;
        }
        d.pixels += u64::from(any);
    }
    d
}

pub(super) fn make_compare(entry: &Entry, p: &Prepared, cx: &PrepareCx<'_>) -> Option<Compare> {
    let (_, out, out_desc, _) = p
        .textures
        .iter()
        .find(|(g, _, _, _)| *g == GameResource::Output)?;
    if out_desc.format.bytes_per_pixel() != 4 {
        return None;
    }
    let shared = |d: &TextureDesc| TextureDesc {
        storage: Storage::Shared,
        shader_write: true,
        shader_read: true,
        ..*d
    };
    let mut twins = Vec::new();
    for r in &entry.written {
        let (_, _, d, _) = p.textures.iter().find(|(g, _, _, _)| g == r)?;
        twins.push((*r, Texture::new(cx.ctx, shared(d)).ok()?));
    }
    let snapshot = Texture::new(cx.ctx, shared(out_desc)).ok()?;
    Some(Compare {
        twins,
        game_output: out.clone(),
        snapshot,
        size: [out_desc.width, out_desc.height, 1],
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// The capture
// ---------------------------------------------------------------------------------------------------------------------

/// `NXKIT_DIAG=ndve`: the list of ndve-native.md section 10 for the run: the keys, and for each dispatch the groups, the
/// storage and uniform buffers (with the offset from the blob start), the push constants, the textures and images with
/// their formats and sizes, and the samplers.
pub(super) fn capture(net: &dyn Replacement, run: Run, cx: &PrepareCx<'_>, hash: u64) {
    let name = net.name();
    eprintln!(
        "DIAG ndve {name}: run of {} dispatches from {}, weight hash {hash:#018x}",
        run.len, run.start
    );
    let base: Vec<Option<u64>> = net
        .blobs()
        .iter()
        .map(|s| {
            cx.detail
                .dispatches
                .get(run.start + s.dispatch)
                .and_then(|d| sbuf_of(d, s.slot))
                .and_then(|b| blob_start(b.address, s.offset))
        })
        .collect();
    for (i, d) in cx
        .detail
        .dispatches
        .iter()
        .enumerate()
        .skip(run.start)
        .take(run.len)
    {
        let n = i - run.start;
        eprintln!(
            "DIAG ndve {name} dispatch {n} key {} groups {:?}",
            d.program, d.groups
        );
        for s in &d.sbufs {
            let rel = base
                .iter()
                .flatten()
                .find_map(|b| s.address.checked_sub(*b).filter(|o| *o < 0x400000));
            eprintln!(
                "DIAG ndve {name}   sbuf slot {} addr {:#x} size {:#x} from blob start {:?}",
                s.slot,
                s.address,
                s.size,
                rel.map(|o| format!("{o:#x}"))
            );
        }
        for u in &d.ubos {
            eprintln!(
                "DIAG ndve {name}   ubo slot {} addr {:#x} size {:#x}",
                u.slot, u.address, u.size
            );
        }
        if let Some(push) = &d.push {
            let words: Vec<String> = push
                .as_chunks::<4>()
                .0
                .iter()
                .map(|w| format!("{:08x}", u32::from_le_bytes(*w)))
                .collect();
            let floats: Vec<f32> = push
                .as_chunks::<4>()
                .0
                .iter()
                .map(|w| f32::from_le_bytes(*w))
                .collect();
            eprintln!(
                "DIAG ndve {name}   push {} bytes words {} floats {floats:?}",
                push.len(),
                words.join(" ")
            );
        }
        for (kind, list) in [("tex", &d.textures), ("img", &d.images)] {
            for t in list {
                let found = cx.view.resolve(t.handle).ok().map(|(slot, tsc)| {
                    let r = cx.textures.get(&slot.texture);
                    (
                        slot.texture,
                        r.map(|r| (r.desc.format, r.desc.width, r.desc.height, r.scaled)),
                        tsc.map(|s| format!("{s:?}")),
                    )
                });
                match found {
                    Some((id, desc, sampler)) => eprintln!(
                        "DIAG ndve {name}   {kind} slot {} handle {:#x} {id:?} {desc:?} sampler {}",
                        t.slot,
                        t.handle,
                        sampler.unwrap_or_else(|| "none".into())
                    ),
                    None => eprintln!(
                        "DIAG ndve {name}   {kind} slot {} handle {:#x} unresolved",
                        t.slot, t.handle
                    ),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The health report
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Counts {
    /// Frames the plan replaced a run in, by network name.
    native: Vec<(&'static str, u64)>,
    /// Frames a found run kept the game's dispatches, by network name and reason, with the first example.
    fallback: Vec<(&'static str, &'static str, u64, String)>,
    /// The counts at the last report.
    reported: Vec<u64>,
}

static COUNTS: Mutex<Counts> = Mutex::new(Counts {
    native: Vec::new(),
    fallback: Vec::new(),
    reported: Vec::new(),
});

fn counts() -> std::sync::MutexGuard<'static, Counts> {
    COUNTS.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn count_native(name: &'static str) {
    let mut c = counts();
    match c.native.iter_mut().find(|(n, _)| *n == name) {
        Some((_, n)) => *n += 1,
        None => c.native.push((name, 1)),
    }
}

pub(super) fn count_fallback(name: &'static str, reason: &'static str, what: &str) {
    static SHOWN: AtomicU32 = AtomicU32::new(0);
    let mut c = counts();
    match c
        .fallback
        .iter_mut()
        .find(|(n, r, _, _)| *n == name && *r == reason)
    {
        Some((_, _, n, _)) => *n += 1,
        None => {
            if crate::diag("ndve") && nxkit_nvn::diag::first(&SHOWN, 50) {
                eprintln!("DIAG ndve {name}: fallback ({reason}): {what}");
            }
            c.fallback
                .push((name, reason, 1, what.chars().take(160).collect()));
        }
    }
}

/// The counters of `ndve.native` and `ndve.fallback` as the lines of the health report, and whether one of them is new
/// since the last report. The totals move on to the next report.
pub(crate) fn health_lines() -> (String, bool) {
    use std::fmt::Write as _;
    let mut c = counts();
    let mut out = String::new();
    let mut fresh = false;
    let mut now = Vec::new();
    for (name, n) in &c.native {
        now.push(*n);
        let _ = writeln!(out, "health:   {:<24} {:>10}", format!("{name}.native"), n);
    }
    for (name, reason, n, example) in &c.fallback {
        now.push(*n);
        let _ = writeln!(
            out,
            "health:   {:<24} {:>10}  e.g. {example}",
            format!("{name}.fallback({reason})"),
            n
        );
    }
    fresh |= now.len() != c.reported.len();
    c.reported = now;
    (out, fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_diff_counts_pixels_and_the_largest_step() {
        let a = [10u8, 20, 30, 255, 0, 0, 0, 255];
        let b = [10u8, 25, 30, 255, 1, 0, 0, 255];
        let d = picture_diff(&a, &b);
        assert_eq!(
            d,
            Diff {
                max: [1, 5, 0, 0],
                pixels: 2,
                total: 2
            }
        );
        assert_eq!(picture_diff(&a, &a).pixels, 0);
    }

    #[test]
    fn the_health_lines_name_native_frames_and_fallback_reasons() {
        count_native("testnet");
        count_fallback(
            "testnet",
            "weights",
            "the bound weights are not the network's",
        );
        let (text, fresh) = health_lines();
        assert!(fresh);
        assert!(text.contains("testnet.native"), "{text}");
        assert!(text.contains("testnet.fallback(weights)"), "{text}");
        let (_, fresh) = health_lines();
        assert!(!fresh);
    }
}
