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
