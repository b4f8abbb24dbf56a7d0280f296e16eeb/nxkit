//! The diagnostics of the encoder.

use super::*;

/// Prints a line of the `target` category, at most 300.
pub(super) fn target_diag(what: std::fmt::Arguments<'_>) {
    static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if crate::diag("target") && crate::diag::first(&SHOWN, 300) {
        eprintln!("DIAG target {what}");
    }
}

thread_local! {
    /// True while the encoded render pass has an attachment that passes `NXKIT_DIAG_TEX`, or always without it. The
    /// per-draw categories print the draws of such passes only.
    pub(super) static FOCUS_PASS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// The index of the render pass that is being encoded, for the lines of the diagnostics.
    pub(super) static DIAG_PASS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The most lines that one per-draw category prints on one sampled frame.
pub(super) const DRAW_LINES: u32 = 2000;

/// Whether a per-draw category prints this draw: on sampled frames, in focused passes, within [`DRAW_LINES`].
pub(super) fn draw_diag(cat: &str, budget: &std::sync::atomic::AtomicU64) -> bool {
    crate::diag(cat)
        && crate::diag::sampled()
        && FOCUS_PASS.with(std::cell::Cell::get)
        && crate::diag::sample_budget(budget, DRAW_LINES)
}

/// The `ubo` category: the uniform buffers of draw `n` as the guest memory holds them now and as the snapshot of the
/// walk holds them, and the first floats of its first two vertex streams.
pub(super) fn ubo_diag(sh: &Shared<'_>, n: u32, d: &crate::detail::DrawDetail) {
    let floats = |addr: u64, count: usize| -> Vec<f32> {
        let mut bytes = vec![0u8; count * 4];
        match sh.guest_offset(addr, bytes.len() as u64) {
            Some(off) if sh.guest.read(off, &mut bytes).is_ok() => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            _ => Vec::new(),
        }
    };
    for u in &d.ubos {
        let count = (u.size as usize / 4).min(16);
        let snap: Option<Vec<f32>> = u.snapshot.as_ref().map(|b| {
            b.as_chunks::<4>()
                .0
                .iter()
                .take(count)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        });
        eprintln!(
            "DIAG ubo draw {n} {:?} slot {} addr {:#x} size {} live {:?} snapshot {snap:?}",
            u.stage,
            u.slot,
            u.address,
            u.size,
            floats(u.address, count)
        );
    }
    for st in d.streams.iter().take(2) {
        eprintln!(
            "DIAG ubo draw {n} stream {} addr {:#x} stride {} floats {:?}",
            st.index,
            st.address,
            st.stride,
            floats(st.address, 8)
        );
    }
}

/// `DIAG ubodump` for a draw of a listed fragment program: once per frame for each program and storage buffer slot
/// (and stage; with the knob `ubodumpall` every draw, by program set, without the words, 600 lines), the words 16 to 31 and the count of non-finite words of the storage buffers of the draw, from the
/// snapshot of the walk (what the shader reads) and from guest memory now. At most 64 lines per frame.
pub(super) fn ubo_dump_draw(sh: &Shared<'_>, n: u32, d: &crate::detail::DrawDetail) {
    use std::sync::atomic::AtomicU64;
    static BUDGET: AtomicU64 = AtomicU64::new(0);
    type Seen = (u64, Vec<(String, u8, u32)>);
    static SEEN: std::sync::Mutex<Seen> =
        std::sync::Mutex::new((0, Vec::new()));
    let all = crate::knob("ubodumpall");
    let fs = d.program.fragment.as_deref();
    let listed = fs.is_some_and(|k| crate::diag::fs_programs().iter().any(|l| l == k));
    if !all && !listed {
        return;
    }
    let max = if all { 600 } else { 64 };
    let key: String = if all {
        format!(
            "{}/{}/{}/{}",
            d.program.vertex,
            fs.unwrap_or("-"),
            d.program.task.as_deref().unwrap_or("-"),
            d.program.mesh.as_deref().unwrap_or("-")
        )
    } else {
        fs.unwrap_or_default().to_owned()
    };
    let key = key.as_str();
    let pass = DIAG_PASS.with(std::cell::Cell::get);
    for s in &d.sbufs {
        let stage = match s.stage {
            crate::detail::Stage::Fragment => "fs",
            crate::detail::Stage::Vertex => "vs",
            crate::detail::Stage::Mesh => "ms",
            crate::detail::Stage::Task => "ts",
            crate::detail::Stage::Compute => "cs",
        };
        {
            let Ok(mut seen) = SEEN.lock() else {
                return;
            };
            let f = crate::diag::frame();
            if seen.0 != f {
                seen.0 = f;
                seen.1.clear();
            }
            let id = (key.to_owned(), s.stage as u8, s.slot);
            if seen.1.contains(&id) {
                continue;
            }
            seen.1.push(id);
        }
        if !crate::diag::sample_budget(&BUDGET, max) {
            return;
        }
        if all {
            eprintln!(
                "DIAG ubodump pass {pass} draw {n} program {key} stage {stage} sbuf {} addr {:#x} size {} snapshot {}",
                s.slot,
                s.address,
                s.size,
                if s.snapshot.is_some() { "yes" } else { "no" }
            );
            continue;
        }
        let count = (s.size as usize / 4).min(4096);
        let live: Option<Vec<u32>> = sh
            .guest_offset(s.address, (count * 4) as u64)
            .and_then(|off| {
                let mut b = vec![0u8; count * 4];
                sh.guest.read(off, &mut b).ok()?;
                Some(
                    b.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| u32::from_le_bytes(*c))
                        .collect(),
                )
            });
        let snap: Option<Vec<u32>> = s.snapshot.as_ref().map(|b| {
            b.as_chunks::<4>()
                .0
                .iter()
                .take(count)
                .map(|c| u32::from_le_bytes(*c))
                .collect()
        });
        let show = |w: &Option<Vec<u32>>| -> String {
            w.as_ref().map_or_else(
                || "unreadable".to_owned(),
                |w| {
                    w.iter()
                        .skip(16)
                        .take(16)
                        .map(|v| format!("{:e}", f32::from_bits(*v)))
                        .collect::<Vec<_>>()
                        .join(" ")
                },
            )
        };
        let bad = |w: &Option<Vec<u32>>| -> usize {
            w.as_ref().map_or(0, |w| {
                w.iter().filter(|v| !f32::from_bits(**v).is_finite()).count()
            })
        };
        let (f32s, nonfinite) = if snap.is_some() {
            (
                format!("snap [{}] guest [{}]", show(&snap), show(&live)),
                format!("{} (guest {})", bad(&snap), bad(&live)),
            )
        } else {
            (format!("[{}]", show(&live)), bad(&live).to_string())
        };
        eprintln!(
            "DIAG ubodump pass {pass} draw {n} fs {key} stage {stage} sbuf {} addr {:#x} size {} snapshot {} f32 {f32s} nonfinite {nonfinite}",
            s.slot,
            s.address,
            s.size,
            if snap.is_some() { "yes" } else { "no" }
        );
    }
}
