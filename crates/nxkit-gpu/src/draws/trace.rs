//! `NXKIT_TRACE_FS=<fragment program key>[:slot,slot,..]`: what one fragment program samples.
//!
//! A draw with a wrong picture often samples a texture that is not the one it should, or one whose guest bytes are
//! empty. For every 300th draw of the program, the trace prints the stencil reference, the depth-stencil state and
//! the scissor. For each listed fragment texture slot (every bound slot without a list) it prints the texture, its
//! format, size, levels, residency and address, and how many of the first 4 KiB of its guest storage are not zero. For
//! a 1x1 texture it prints the four floats too.
//!
//! The handle map keeps only the last texture of a handle, and a game reuses a handle slot from draw to draw. Read the
//! lines as the state of this draw, not of the frame.

use std::sync::atomic::AtomicU32;

use super::Shared;
use crate::detail::{DrawDetail, Stage};
use crate::settings::FsTrace;

/// The draws of the program between two traces.
const EVERY: u32 = 300;

/// Prints the trace of draw `d` when its fragment program is the one that `trace` names.
pub(crate) fn trace_draw(sh: &Shared<'_>, d: &DrawDetail, trace: &FsTrace) {
    // Process-wide (W26): a rate limit of a diagnostic has no context to live in.
    static N: AtomicU32 = AtomicU32::new(0);
    let Some(key) = d.program.fragment.as_deref() else {
        return;
    };
    if key != trace.program || !nxkit_nvn::diag::every(&N, EVERY) {
        return;
    }
    eprintln!(
        "DIAG tracefs {key} stencil ref {:?} depth-stencil {:?} scissor {:?}",
        d.dynamic.stencil_ref, d.depth_stencil, d.dynamic.scissor
    );
    let bound = d
        .textures
        .iter()
        .filter(|t| t.stage == Stage::Fragment && trace.wants(t.slot));
    for t in bound {
        let id = crate::diag::texture_of(t.handle);
        let rec = id.and_then(|id| sh.table.record(id).ok());
        let Some(r) = rec else {
            eprintln!(
                "DIAG tracefs {key} slot {} handle {:#x} -> {id:?} (no record)",
                t.slot, t.handle
            );
            continue;
        };
        let d = &r.desc;
        let mut head = [0u8; 4096];
        let read = sh.read_guest(r.address.get(), &mut head);
        let nonzero = head.iter().filter(|b| **b != 0).count();
        eprintln!(
            "DIAG tracefs {key} slot {} handle {:#x} -> {id:?} {:?} {}x{}x{} levels {} residency {:?} address {:?} guest read {read} nonzero of first 4096 {nonzero}",
            t.slot,
            t.handle,
            d.format,
            d.width,
            d.height,
            d.depth,
            d.levels,
            r.residency,
            r.address
        );
        if d.width == 1 && d.height == 1 {
            eprintln!(
                "DIAG tracefs {key} slot {} 1x1 floats {:?} first written in pass {:?}",
                t.slot,
                floats(&head),
                id.and_then(|i| sh.first_write.get(&i))
            );
        }
    }
}

/// The first four little-endian floats of `bytes`.
fn floats(bytes: &[u8]) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (o, c) in out.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *o = f32::from_le_bytes(*c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_floats_are_read_little_endian() {
        let mut b = Vec::new();
        for f in [1.0f32, -2.5, 0.0, 8.0] {
            b.extend_from_slice(&f.to_le_bytes());
        }
        assert_eq!(floats(&b), [1.0, -2.5, 0.0, 8.0]);
        assert_eq!(floats(&b[..6]), [1.0, 0.0, 0.0, 0.0]);
    }
}
