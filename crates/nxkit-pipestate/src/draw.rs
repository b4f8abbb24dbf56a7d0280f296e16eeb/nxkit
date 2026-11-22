//! The descriptor rules of a draw: which entry functions a pipeline uses and what its colour attachments are.
//!
//! The host builds the pipeline of a draw with these rules (`nxkit-gpu` `pipelines/render.rs`), and the build resolves a
//! seed with them (`crate::seeds`). A seed that the build archived is then the pipeline that the draw asks for.

use crate::format::Format;
use crate::nvn::{Blend, color_target, is_integer};
use crate::program::{ENTRY_LAYER, ENTRY_LAYER_Z, ENTRY_Z};
use crate::state::ColorTarget;

/// The fragment function for the colour formats of the targets: the unsigned variant when every written target is an
/// integer format, the mixed variant when its mask is the integer targets, else the plain entry. `written` lists the
/// colours the shader writes (`None`: all).
pub fn fragment_entry<'a>(
    entry: &'a str,
    entry_uint: Option<&'a str>,
    entry_mixed: Option<(&'a str, u8)>,
    written: Option<&[u8]>,
    colors: &[Option<Format>],
) -> &'a str {
    let mut present = 0u8;
    let mut integer = 0u8;
    for (slot, format) in colors.iter().enumerate() {
        let Some(format) = format else { continue };
        if written.is_some_and(|w| !w.contains(&(slot as u8))) {
            continue;
        }
        present |= 1 << slot;
        if is_integer(*format) {
            integer |= 1 << slot;
        }
    }
    if integer != 0 {
        if integer == present {
            if let Some(entry) = entry_uint {
                return entry;
            }
        } else if let Some((entry, mask)) = entry_mixed
            && mask & present == integer
        {
            return entry;
        }
    }
    entry
}

/// The colour attachments of a draw. `fragment` is `None` for a draw without a fragment shader, else the colours the
/// shader writes (`None`: all). A slot without a target format is empty.
pub fn color_targets(
    blend: &[Blend],
    write_masks: &[u8],
    fragment: Option<Option<&[u8]>>,
    colors: &[Option<Format>],
) -> Vec<Option<ColorTarget>> {
    colors
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let format = (*f)?;
            let written = fragment.is_some_and(|w| w.is_none_or(|c| c.contains(&(i as u8))));
            let b = blend.get(i).copied().unwrap_or_default();
            let mask = write_masks.get(i).copied().unwrap_or(0xF);
            Some(color_target(format, &b, mask, written))
        })
        .collect()
}

/// The vertex function a draw runs and the suffix its pipeline key adds to the shader key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexEntry<'a> {
    /// The entry function.
    pub entry: &'a str,
    /// `""`, `":z"`, `":layer"` or `":layer:z"`.
    pub suffix: &'static str,
    /// The device keeps the clip depth, and the shader has no entry for it: the entry converts the depth.
    pub depth_mode_ignored: bool,
}

/// The vertex function of a draw: the layer entry for a `layered` draw, the base entry otherwise, and with the
/// device in the depth mode 0 to 1 (`zero_to_one`) the form of the entry that keeps the clip depth (`entry_z[0]` for
/// the base entry, `entry_z[1]` for the layer entry). The base forms convert the -1 to 1 depth of the default mode.
pub fn vertex_entry<'a>(
    layered: bool,
    entry: &'a str,
    entry_layer: Option<&'a str>,
    entry_z: [Option<&'a str>; 2],
    zero_to_one: bool,
) -> VertexEntry<'a> {
    let (plain, kept) = if layered {
        (entry_layer.unwrap_or(entry), entry_z[1])
    } else {
        (entry, entry_z[0])
    };
    let (entry, suffix, depth_mode_ignored) = match (zero_to_one, kept) {
        (true, Some(z)) => (z, if layered { ":layer:z" } else { ":z" }, false),
        (true, None) => (plain, if layered { ":layer" } else { "" }, true),
        (false, _) => (plain, if layered { ":layer" } else { "" }, false),
    };
    VertexEntry {
        entry,
        suffix,
        depth_mode_ignored,
    }
}

/// The suffix of the key of a vertex entry, from the name the translator gives it ([`vertex_entry`]). A function of a
/// pulling library has none: it has only its pulling entry.
pub fn vertex_suffix(entry: &str, pull: bool) -> &'static str {
    match entry {
        _ if pull => "",
        ENTRY_Z => ":z",
        ENTRY_LAYER => ":layer",
        ENTRY_LAYER_Z => ":layer:z",
        _ => "",
    }
}

/// The mesh function of a mesh draw: `entry_z` with the device in the depth mode 0 to 1, see [`vertex_entry`]. The
/// flag is true when the device keeps the depth and the shader has no entry for it.
pub fn mesh_entry<'a>(
    entry: &'a str,
    entry_z: Option<&'a str>,
    zero_to_one: bool,
) -> (&'a str, bool) {
    match (zero_to_one, entry_z) {
        (true, Some(z)) => (z, false),
        (true, None) => (entry, true),
        (false, _) => (entry, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_integer_target_takes_the_unsigned_or_mixed_entry() {
        let colors = [Some(Format::R32Uint), Some(Format::Rgba8Unorm)];
        assert_eq!(
            fragment_entry("main0", Some("main0_u"), None, Some(&[0]), &colors),
            "main0_u"
        );
        assert_eq!(
            fragment_entry(
                "main0",
                Some("main0_u"),
                Some(("main0_m", 1)),
                None,
                &colors
            ),
            "main0_m"
        );
        assert_eq!(
            fragment_entry("main0", Some("main0_u"), None, None, &colors),
            "main0"
        );
    }

    #[test]
    fn the_vertex_entry_follows_the_depth_mode_and_the_layer() {
        let z = [Some(ENTRY_Z), Some(ENTRY_LAYER_Z)];
        let e = vertex_entry(false, "main0", Some(ENTRY_LAYER), z, true);
        assert_eq!((e.entry, e.suffix), (ENTRY_Z, ":z"));
        assert_eq!(vertex_suffix(e.entry, false), e.suffix);
        let e = vertex_entry(true, "main0", Some(ENTRY_LAYER), z, true);
        assert_eq!((e.entry, e.suffix), (ENTRY_LAYER_Z, ":layer:z"));
        assert_eq!(vertex_suffix(e.entry, false), e.suffix);
        let e = vertex_entry(true, "main0", Some(ENTRY_LAYER), z, false);
        assert_eq!(vertex_suffix(e.entry, false), e.suffix);
        let e = vertex_entry(false, "main0", None, [None, None], true);
        assert!(e.depth_mode_ignored);
        assert_eq!((e.entry, e.suffix), ("main0", ""));
    }
}
