//! `NXKIT_DIAG=rendercond`, the commands that a render condition gates.
//!
//! The front end records, for each run of commands under a conditional render, the condition that covered it
//! (`nxkit_nvn::rendercond::gate`). On a sampled frame [`frame`] prints what the commands are. It takes the state of a
//! draw from the detail of the frame, which the walker records for every draw, also for one that the condition left
//! out, so a command that is not encoded is described the same way as one that is. It prints a line for each distinct
//! (kind, fragment program, target format, condition source, decision) and at most 200 lines per run.
//!
//! The conditions whose operands both are plain guest memory (a flag) are held until the GPU finished their frame
//! ([`hold`], [`landed`]). Then the 32 bytes are read again, to see whether the memory changed after the host
//! decided. [`frame`] also prints the bindings of the frame that overlap those 32 bytes.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};

use nxkit_metal::GuestSpace;
use nxkit_nvn::counters::CounterWrite;
use nxkit_nvn::frame::{Cmd, RtSet, conditional};
use nxkit_nvn::rendercond::Read;
use nxkit_nvn::rendercond::force::Source;
use nxkit_nvn::rendercond::gate::Info;
use nxkit_nvn::resource::TexId;

use crate::detail::FrameDetail;
use crate::draws::Shared;

/// The most `gate` lines of a run.
const MAX_GATES: u32 = 200;
/// The most `flag` lines of a frame.
const MAX_FLAGS: usize = 40;

/// A condition on two words of guest memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Flag {
    addr: u64,
    /// The words when the front end decided.
    at_append: [u64; 2],
}

static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static LINES: AtomicU32 = AtomicU32::new(0);
static PENDING: Mutex<Vec<Flag>> = Mutex::new(Vec::new());
static HELD: Mutex<Vec<(u64, Vec<Flag>)>> = Mutex::new(Vec::new());
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The kind of a gated command, None for a command that no condition leaves out.
fn kind(c: &Cmd) -> Option<&'static str> {
    match c {
        Cmd::Draw { .. } => Some("draw"),
        Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => Some("clear"),
        Cmd::Dispatch { .. } => Some("dispatch"),
        Cmd::Copy { .. } | Cmd::BufferCopy | Cmd::Downsample { .. } => Some("copy"),
        _ => None,
    }
}

/// A read of a condition as `kind:value`.
fn read_text(r: Read) -> String {
    r.value()
        .map_or_else(|| r.kind().to_owned(), |v| format!("{}:{v:x}", r.kind()))
}

/// The name of an `NVNdepthFunc`.
fn depth_func(f: u32) -> &'static str {
    [
        "?", "never", "less", "equal", "lequal", "greater", "notequal", "gequal", "always",
    ]
    .get(f as usize)
    .copied()
    .unwrap_or("?")
}

/// The state of a command as the lines print it.
struct Described {
    prog: String,
    blend: String,
    depth: String,
    write: &'static str,
    mask: String,
    extra: String,
}

impl Described {
    fn none(prog: String) -> Self {
        let dash = || "-".to_owned();
        Self {
            prog,
            blend: dash(),
            depth: dash(),
            write: "-",
            mask: dash(),
            extra: String::new(),
        }
    }
}

fn describe(sh: &Shared<'_>, i: u32, cmd: &Cmd, slot: usize) -> Described {
    let o = sh.ordinals.get(i as usize).copied().unwrap_or_default();
    match cmd {
        Cmd::Draw { .. } => {
            let Some(d) = sh.detail.draws.get(o.draw as usize) else {
                return Described::none("no-detail".into());
            };
            let b = d.blend[slot.min(7)];
            let ds = &d.depth_stencil;
            Described {
                prog: d.program.fragment.as_deref().unwrap_or("-").to_owned(),
                blend: if b.enable {
                    format!("on {}/{}", b.src_rgb, b.dst_rgb)
                } else {
                    "off".into()
                },
                depth: if ds.depth_test {
                    depth_func(ds.depth_func).into()
                } else {
                    "off".into()
                },
                write: if ds.depth_write { "on" } else { "off" },
                mask: format!("{:x}", d.write_masks[slot.min(7)]),
                extra: String::new(),
            }
        }
        Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => {
            let mut d = Described::none("-".into());
            if let Some(c) = sh.detail.clears.get(o.clear as usize) {
                d.extra = if matches!(cmd, Cmd::ClearColor { .. }) {
                    format!(" clear color {:?}", c.color)
                } else {
                    format!(" clear depth {} stencil {}", c.depth, c.stencil)
                };
            }
            d
        }
        Cmd::Dispatch { .. } => Described::none(
            sh.detail
                .dispatches
                .get(o.dispatch as usize)
                .map_or_else(|| "no-detail".into(), |d| d.program.clone()),
        ),
        _ => Described::none("-".into()),
    }
}

/// The format and size of a texture: the Metal texture when the frame has one, else the record of the game.
fn target_text(sh: &Shared<'_>, id: Option<TexId>) -> String {
    let Some(id) = id else {
        return "- 0x0".into();
    };
    if let Some(r) = sh.textures.get(&id) {
        return format!("{:?} {}x{}", r.desc.format, r.desc.width, r.desc.height);
    }
    sh.table.record(id).map_or_else(
        |_| "unknown 0x0".into(),
        |r| format!("nvn{} {}x{}", r.desc.format, r.desc.width, r.desc.height),
    )
}

/// The target of a command: the first colour target of the render targets set before it, else the depth target. A
/// copy has its destination. Also the slot of the colour target.
fn target_of(cmd: &Cmd, rt: Option<&RtSet>) -> (Option<TexId>, usize) {
    match cmd {
        Cmd::Copy { dst, .. } => (*dst, 0),
        Cmd::Downsample { dst, .. } => (Some(*dst), 0),
        Cmd::Draw { .. } | Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => {
            let Some(rt) = rt else { return (None, 0) };
            match rt.color.iter().position(Option::is_some) {
                Some(s) => (rt.color[s], s),
                None => (rt.depth, 0),
            }
        }
        _ => (None, 0),
    }
}

/// Whether the draws between the reset of the counter and the report that `info` names lie in the pass `pass`.
fn same_pass(
    sh: &Shared<'_>,
    info: &Info,
    pass: Option<usize>,
    pass_of: &dyn Fn(u32) -> Option<usize>,
) -> &'static str {
    let c = &sh.frame.counters;
    let w: Option<&CounterWrite> = c
        .writes
        .iter()
        .rev()
        .find(|w| w.addr == info.addr || w.addr == info.addr.wrapping_add(16));
    let (Some(w), Some(pass)) = (w, pass) else {
        return "unknown";
    };
    let slots = c.sums_of(w);
    let mut draws = 0u32;
    for (range, slot) in &c.ranges {
        if !slots.contains(slot) {
            continue;
        }
        for i in range.clone() {
            if matches!(sh.frame.cmds.get(i as usize), Some(Cmd::Draw { .. })) {
                draws += 1;
                if pass_of(i) != Some(pass) {
                    return "no";
                }
            }
        }
    }
    if draws == 0 { "unknown" } else { "yes" }
}

/// Prints the `gate` lines of a sampled frame, and the writers of the flags. Keeps the flags for [`hold`].
pub(crate) fn frame(sh: &Shared<'_>) {
    let f = sh.frame;
    if f.gates.is_empty() {
        return;
    }
    let pass_of = |i: u32| {
        let at = sh.plan.passes.partition_point(|p| p.cmds.end <= i);
        sh.plan
            .passes
            .get(at)
            .filter(|p| p.cmds.start <= i)
            .map(|_| at)
    };
    // The render targets set before each command.
    let mut last_rt: Vec<Option<u32>> = Vec::with_capacity(f.cmds.len());
    let mut cur = None;
    for (i, c) in f.cmds.iter().enumerate() {
        if matches!(c, Cmd::SetRenderTargets(_)) {
            cur = Some(i as u32);
        }
        last_rt.push(cur);
    }
    let mut flags: Vec<Flag> = Vec::new();
    for seg in &f.gates {
        let info = &seg.info;
        if info.source == Source::Memory
            && flags.len() < MAX_FLAGS
            && !flags.iter().any(|g| g.addr == info.addr)
        {
            let v = |k: usize| info.reads[k].value().unwrap_or(0);
            flags.push(Flag {
                addr: info.addr,
                at_append: [v(0), v(1)],
            });
        }
        for i in seg.range.clone() {
            let Some(cmd) = f.cmds.get(i as usize) else {
                continue;
            };
            let Some(what) = kind(cmd).filter(|_| conditional(cmd)) else {
                continue;
            };
            let rt = last_rt.get(i as usize).copied().flatten().and_then(|k| {
                match f.cmds.get(k as usize) {
                    Some(Cmd::SetRenderTargets(rt)) => Some(rt),
                    _ => None,
                }
            });
            let (tex, slot) = target_of(cmd, rt);
            let d = describe(sh, i, cmd, slot);
            let target = target_text(sh, tex);
            let decision = info.decision_name();
            let key = format!(
                "{what} {} {target} {} {decision}",
                d.prog,
                info.source.name()
            );
            let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
            if !seen.get_or_insert_with(HashSet::new).insert(key)
                || LINES.load(Ordering::Relaxed) >= MAX_GATES
            {
                continue;
            }
            drop(seen);
            LINES.fetch_add(1, Ordering::Relaxed);
            let pass = pass_of(i);
            let pass_text = pass.map_or_else(
                || "- none".to_owned(),
                |p| format!("{p} {:?}", sh.plan.passes[p].kind),
            );
            let samepass = if info.source == Source::Counter {
                format!(" samepass {}", same_pass(sh, info, pass, &pass_of))
            } else {
                String::new()
            };
            eprintln!(
                "DIAG rendercond gate {what} pass {pass_text} prog {} blend {} depth test {} write {} mask {} target {target} cond {} decision {decision} addr {:#x}{samepass} mode {} reads {} {}{}",
                d.prog,
                d.blend,
                d.depth,
                d.write,
                d.mask,
                info.source.name(),
                info.addr,
                info.mode,
                read_text(info.reads[0]),
                read_text(info.reads[1]),
                d.extra,
            );
        }
    }
    if !flags.is_empty() {
        writers(sh.detail, &flags);
        *PENDING.lock().unwrap_or_else(PoisonError::into_inner) = flags;
    }
}

/// Prints the bindings and copy destinations of the frame that overlap the 32 bytes of a flag.
fn writers(detail: &FrameDetail, flags: &[Flag]) {
    let hit = |start: u64, end: u64| flags.iter().find(|g| start < g.addr + 32 && g.addr < end);
    let mut seen: HashSet<(&'static str, u64, u64)> = HashSet::new();
    let mut say = |kind: &'static str, start: u64, end: u64| {
        if let Some(g) = hit(start, end)
            && seen.insert((kind, start, end))
            && seen.len() <= MAX_FLAGS
        {
            eprintln!(
                "DIAG rendercond flag writer {kind} {start:#x}..{end:#x} (flag {:#x})",
                g.addr
            );
        }
    };
    for d in &detail.draws {
        for u in &d.sbufs {
            say("storage-buffer", u.address, u.address + u64::from(u.size));
        }
        for u in &d.ubos {
            say(
                "uniform-buffer-bound",
                u.address,
                u.address + u64::from(u.size),
            );
        }
    }
    for d in &detail.dispatches {
        for u in &d.sbufs {
            say("storage-buffer", u.address, u.address + u64::from(u.size));
        }
    }
    for c in &detail.copies {
        if c.buffer == 0 {
            continue;
        }
        // The size of the buffer side is estimated from the strides; a copy without strides counts its first byte.
        let len = match (c.image_stride, c.row_stride) {
            (s, _) if s > 0 => s * u64::from(c.size[2].max(1)),
            (_, r) if r > 0 => r * u64::from(c.size[1].max(1)) * u64::from(c.size[2].max(1)),
            _ => 1,
        };
        say("copy-buffer", c.buffer, c.buffer + len);
    }
}

/// The flags of the frame that was just encoded belong to `fence`. They wait for the GPU.
pub(crate) fn hold(fence: u64) {
    let mut p = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
    if p.is_empty() {
        return;
    }
    let flags = std::mem::take(&mut *p);
    drop(p);
    ACTIVE.store(true, Ordering::Release);
    HELD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((fence, flags));
}

/// The GPU finished the frames up to fence `upto`: reads the flags of those frames again.
pub(crate) fn landed(upto: u64, guest: &GuestSpace) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let due: Vec<Flag> = {
        let mut h = HELD.lock().unwrap_or_else(PoisonError::into_inner);
        let (due, keep): (Vec<_>, Vec<_>) = h.drain(..).partition(|(f, _)| *f <= upto);
        *h = keep;
        due.into_iter().flat_map(|(_, f)| f).collect()
    };
    let map = guest.address_map();
    for g in due {
        let mut b = [0u8; 32];
        let ok = map
            .host_address(g.addr, 32)
            .and_then(|h| map.offset_of_host(h, 32))
            .and_then(|off| guest.read(off, &mut b))
            .is_ok();
        let word =
            |k: usize| u64::from_le_bytes(b[k * 16..k * 16 + 8].try_into().unwrap_or([0; 8]));
        let [a, c] = g.at_append;
        if ok {
            eprintln!(
                "DIAG rendercond flag addr {:#x} at-append {a:x},{c:x} after-gpu {:x},{:x}",
                g.addr,
                word(0),
                word(1)
            );
        } else {
            eprintln!(
                "DIAG rendercond flag addr {:#x} at-append {a:x},{c:x} after-gpu outside guest memory",
                g.addr
            );
        }
    }
}
