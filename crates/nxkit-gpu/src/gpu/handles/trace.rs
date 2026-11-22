//! The traces of the handle resolve: `NXKIT_TRACE_HANDLES` and `NXKIT_DIAG=border`.
//!
//! A bindless handle names a texture and a sampler through the tables of the game. A game reuses a handle slot from draw
//! to draw, so the line of a handle shows what the slot held when the frame resolved it, not what every draw saw.

use std::sync::{Mutex, PoisonError};

use nxkit_metal::Border;
use nxkit_nvn::resource::TexId;
use nxkit_nvn::tic::{Tic, Tsc};

use crate::snapshot::EncodeView;
use crate::textures::Res;

/// The resolves of a traced handle that print first, before the lines get rarer.
const FIRST: u32 = 4;
/// After [`FIRST`], every this many resolves of a traced handle print.
const EVERY: u32 = 60;
/// The (texture, colour) pairs that `border` prints at most, in one generation of the live settings.
const MAX_BORDERS: usize = 200;

/// A texture and the bits of a border colour.
type Seen = (TexId, [u32; 4]);

/// The generation of the live settings, and the resolves counted for each traced handle in it.
// Process-wide (W26): the counts of a diagnostic have no context to live in.
static COUNTS: Mutex<(u64, Vec<(u64, u32)>)> = Mutex::new((0, Vec::new()));
/// The generation of the live settings, and the (texture, colour) pairs that `border` printed in it.
// Process-wide (W26): the list of a diagnostic has no context to live in.
static BORDERS: Mutex<(u64, Vec<Seen>)> = Mutex::new((0, Vec::new()));

/// Counts a resolve of `handle` and says whether it prints: one of the first [`FIRST`], then every [`EVERY`]th.
fn due(handle: u64) -> bool {
    let mut g = COUNTS.lock().unwrap_or_else(PoisonError::into_inner);
    let now = nxkit_abi::live::generation();
    if g.0 != now {
        *g = (now, Vec::new());
    }
    let at = match g.1.iter().position(|(h, _)| *h == handle) {
        Some(i) => i,
        None => {
            g.1.push((handle, 0));
            g.1.len() - 1
        }
    };
    let n = g.1[at].1;
    g.1[at].1 = n.saturating_add(1);
    n < FIRST || n.is_multiple_of(EVERY)
}

/// `NXKIT_TRACE_HANDLES`: the texture, its description, the header (format, view with the swizzle), the residency, the
/// address and the flags of a handle that the list names.
pub(super) fn trace_handle(view: &dyn EncodeView, handle: u64, id: TexId, res: &Res, header: &Tic) {
    if !crate::settings::live().trace_handles.contains(&handle) || !due(handle) {
        return;
    }
    let rec = view.record(id).ok();
    eprintln!(
        "DIAG trace-handle {handle:#x} -> {id:?} desc {:?} tic format {:#x} view {:?} residency {:?} address {:?} flags {:?}",
        res.desc,
        header.format,
        header.view,
        rec.map(|r| r.residency),
        rec.map(|r| r.address),
        rec.map(|r| r.desc.flags)
    );
}

/// Whether a wrap mode of the sampler header is clamp to border.
fn clamps_to_border(w: u8) -> bool {
    matches!(w, 4 | 5)
}

/// Whether Metal has this border colour: only transparent black, opaque black and opaque white exist.
pub(crate) fn metal_border_exact(c: [f32; 4]) -> bool {
    c == [0.0; 4] || c == [0.0, 0.0, 0.0, 1.0] || c == [1.0; 4]
}

/// `NXKIT_DIAG=border`: once for each texture and border colour, a sampler that clamps to the border, the colour, and
/// whether Metal can show it. The colour that the sampler gets is the nearest of the three that Metal has. The
/// `border` binding of a program always reads the zero block, and the `_w` twin of a sampler gets the sampler of its
/// main one, so a custom colour does not reach a shader that reads it (`docs/nxkit-gpu.md`, "Limits").
pub(super) fn border_diag(handle: u64, id: TexId, res: &Res, tsc: Option<&Tsc>) {
    let Some(t) = tsc else {
        return;
    };
    let wraps = [t.wrap_s, t.wrap_t, t.wrap_r];
    if !wraps.iter().any(|w| clamps_to_border(*w)) || !crate::diag("border") {
        return;
    }
    let key = (id, t.border.map(f32::to_bits));
    let mut seen = BORDERS.lock().unwrap_or_else(PoisonError::into_inner);
    let now = nxkit_abi::live::generation();
    if seen.0 != now {
        *seen = (now, Vec::new());
    }
    if seen.1.len() >= MAX_BORDERS || seen.1.contains(&key) {
        return;
    }
    seen.1.push(key);
    let exact = metal_border_exact(t.border);
    let used: Border = crate::translate::border(t.border);
    eprintln!(
        "DIAG border handle {handle:#x} {id:?} {}x{} {:?} wrap {wraps:?} colour {:?} metal exact {exact} sampler gets {used:?}, \
         but the border binding reads the zero block and a _w twin gets the main sampler: the colour is not applied there",
        res.desc.width, res.desc.height, res.desc.format, t.border
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metal_has_three_border_colours() {
        assert!(metal_border_exact([0.0; 4]));
        assert!(metal_border_exact([0.0, 0.0, 0.0, 1.0]));
        assert!(metal_border_exact([1.0; 4]));
        assert!(!metal_border_exact([0.5, 0.5, 0.5, 1.0]));
        assert!(!metal_border_exact([1.0, 1.0, 1.0, 0.0]));
    }

    #[test]
    fn only_the_clamp_to_border_wraps_count() {
        assert!(clamps_to_border(4) && clamps_to_border(5));
        assert!(!clamps_to_border(0) && !clamps_to_border(1) && !clamps_to_border(6));
    }

    #[test]
    fn a_traced_handle_prints_four_times_and_then_every_sixtieth() {
        let h = 0xdead_0001;
        let hits = (0..130).filter(|_| due(h)).count();
        assert_eq!(hits, 6);
    }
}
