//! The capture list of `NXKIT_DIAG=<name>`: what the game binds in a run, printed once per distinct run.

use super::*;

/// The list of `ndve-native.md` / `ndsr-native.md` section 10 for the run: the keys, and for each dispatch the groups,
/// the storage and uniform buffers (with the offset from the blob start), the push constants, the textures and images
/// with their formats and sizes, the samplers, and whether a binding is a texture another dispatch of the list
/// binds as well. Then, for the textures the run's last dispatch binds, every command of the frame that reads or writes
/// them. A replacement that names a `lead` gets that many dispatches before the run in the list.
pub(super) fn capture(
    net: &dyn Replacement,
    shape: &Shape,
    run: Run,
    cx: &PrepareCx<'_>,
    hash: u64,
    anchor_cmd: usize,
) {
    let name = net.name();
    eprintln!(
        "DIAG {name} {name}: run of {} dispatches from {}, weight hash {hash:#018x}, tag {:?}",
        run.len, run.start, shape.tag
    );
    let base: Vec<Option<u64>> = shape
        .blobs
        .iter()
        .map(|s| {
            cx.detail
                .dispatches
                .get(run.start + s.dispatch)
                .and_then(|d| sbuf_of(d, s.slot))
                .and_then(|b| blob_start(b.address, s.offset))
        })
        .collect();
    let first = run.start.saturating_sub(net.lead());
    // The texture each earlier binding of the list resolved to: (dispatch, kind, slot, backing).
    let mut seen: Vec<(isize, &str, u32, Option<_>)> = Vec::new();
    let mut last_bound: Vec<TexId> = Vec::new();
    for (i, d) in cx
        .detail
        .dispatches
        .iter()
        .enumerate()
        .skip(first)
        .take(run.start + run.len - first)
    {
        let n = i as isize - run.start as isize;
        eprintln!(
            "DIAG {name} {name} dispatch {n} key {} groups {:?}",
            d.program, d.groups
        );
        for s in &d.sbufs {
            let rel = base
                .iter()
                .flatten()
                .find_map(|b| s.address.checked_sub(*b).filter(|o| *o < 0x400000));
            eprintln!(
                "DIAG {name} {name}   sbuf slot {} addr {:#x} size {:#x} from blob start {:?}",
                s.slot,
                s.address,
                s.size,
                rel.map(|o| format!("{o:#x}"))
            );
        }
        for u in &d.ubos {
            eprintln!(
                "DIAG {name} {name}   ubo slot {} addr {:#x} size {:#x}",
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
                "DIAG {name} {name}   push {} bytes words {} floats {floats:?}",
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
                        r.map(|r| r.barrier_id),
                        tsc.map(|s| format!("{s:?}")),
                    )
                });
                match found {
                    Some((id, desc, backing, sampler)) => {
                        let same: Vec<String> = seen
                            .iter()
                            .filter(|(_, _, _, b)| backing.is_some() && *b == backing)
                            .map(|(dn, k, s, _)| format!("{k} slot {s} of dispatch {dn}"))
                            .collect();
                        eprintln!(
                            "DIAG {name} {name}   {kind} slot {} handle {:#x} {id:?} {desc:?} sampler {} same texture as [{}]",
                            t.slot,
                            t.handle,
                            sampler.unwrap_or_else(|| "none".into()),
                            same.join(", ")
                        );
                        seen.push((n, kind, t.slot, backing));
                        if i + 1 == run.start + run.len {
                            last_bound.push(id);
                        }
                    }
                    None => eprintln!(
                        "DIAG {name} {name}   {kind} slot {} handle {:#x} unresolved",
                        t.slot, t.handle
                    ),
                }
            }
        }
    }
    for id in last_bound {
        let users = users_of(cx, id);
        eprintln!(
            "DIAG {name} {name}   users of {id:?} in the frame (network dispatch is cmd {anchor_cmd}): {}",
            if users.is_empty() {
                "none".into()
            } else {
                users.join("; ")
            }
        );
    }
}

/// The commands of the frame that read or write the texture `id` or another view of its backing, as sentences: the
/// command index, what it does, and the program key of a dispatch.
fn users_of(cx: &PrepareCx<'_>, id: TexId) -> Vec<String> {
    let backing = |t: &TexId| cx.textures.get(t).map(|res| res.barrier_id);
    let want = backing(&id);
    let same = |t: &TexId| want.is_some() && backing(t) == want;
    let mut out = Vec::new();
    let mut dispatch = 0usize;
    for (i, c) in cx.frame.cmds.iter().enumerate() {
        let what = match *c {
            Cmd::Draw { reads, .. } if cx.frame.list(reads).iter().any(same) => "draw reads".into(),
            Cmd::Dispatch { reads, writes } => {
                let key = cx
                    .detail
                    .dispatches
                    .get(dispatch)
                    .map_or("?", |d| d.program.as_str());
                dispatch += 1;
                let r = cx.frame.list(reads).iter().any(same);
                let w = cx.frame.list(writes).iter().any(same);
                match (r, w) {
                    (true, true) => format!("dispatch {} {key} reads and writes", dispatch - 1),
                    (true, false) => format!("dispatch {} {key} reads", dispatch - 1),
                    (false, true) => format!("dispatch {} {key} writes", dispatch - 1),
                    (false, false) => continue,
                }
            }
            Cmd::Copy { src, dst } => {
                match (src.is_some_and(|t| same(&t)), dst.is_some_and(|t| same(&t))) {
                    (true, _) => "copy reads".into(),
                    (_, true) => "copy writes".into(),
                    _ => continue,
                }
            }
            Cmd::Downsample { src, .. } if same(&src) => "downsample reads".into(),
            Cmd::Present(t) if same(&t) => "present".into(),
            Cmd::CpuRead(t) if same(&t) => "cpu read".into(),
            _ => continue,
        };
        out.push(format!("cmd {i} {what}"));
        if out.len() >= 16 {
            out.push("...".into());
            break;
        }
    }
    out
}
