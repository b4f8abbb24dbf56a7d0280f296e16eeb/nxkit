//! What a capture holds: the passes of a frame as the hooks saw them and the images that were read back.

use nxkit_metal::Format;

use super::Want;

/// One blend state of a colour slot that a draw of the pass used. The names are the Metal factors and operations that
/// the NVN values map to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlendRec {
    pub(crate) enable: bool,
    pub(crate) src_rgb: String,
    pub(crate) dst_rgb: String,
    pub(crate) op_rgb: String,
    pub(crate) src_alpha: String,
    pub(crate) dst_alpha: String,
    pub(crate) op_alpha: String,
    /// The colour write mask: bit 0 red, 1 green, 2 blue, 3 alpha.
    pub(crate) write_mask: u8,
}

/// One attachment of a render pass.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AttRec {
    /// `c0` to `c7` for a colour slot, `depth` for depth and stencil.
    pub(crate) label: String,
    pub(crate) tex: u32,
    pub(crate) format: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) load: String,
    pub(crate) store: String,
    /// The clear colour (four values) or the clear depth (one value) of a pass that clears the attachment.
    pub(crate) clear: Option<Vec<f64>>,
    /// Why the attachment has no picture, when the encoder knows it before the read back.
    pub(crate) skipped: Option<String>,
}

/// One pass of the plan.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PassRec {
    pub(crate) index: usize,
    /// The kind of the plan: `Render`, `Compute`, `Copy`, `Sync`, `Present` or `CpuRead`.
    pub(crate) kind: String,
    /// The first and the end command of the pass.
    pub(crate) cmds: [u32; 2],
    pub(crate) draws: u32,
    pub(crate) attachments: Vec<AttRec>,
    pub(crate) vertex: Vec<String>,
    pub(crate) fragment: Vec<String>,
    pub(crate) task: Vec<String>,
    pub(crate) mesh: Vec<String>,
    /// The distinct blend states by colour slot, for the slots that the pass attaches.
    pub(crate) blend: Vec<(u8, Vec<BlendRec>)>,
}

/// What an image shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    /// The presented picture of the frame.
    Final,
    /// An attachment of a render pass after the pass.
    Pass(usize, String),
    /// A texture by id, as the frame left it.
    Texture(u32),
}

/// A texture read back.
#[derive(Debug)]
pub(crate) struct Image {
    pub(crate) key: Key,
    /// The id of the texture of the game, when known.
    pub(crate) tex: Option<u32>,
    pub(crate) format: Format,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The bytes of level 0 and layer 0, or why there are none.
    pub(crate) data: Result<Vec<u8>, String>,
}

/// The numbers of the frame that the encoder knows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameInfo {
    pub(crate) encode_ns: u64,
    /// The GPU time of the frame's command buffers, when the wait for the frame gave it.
    pub(crate) gpu_ns: Option<u64>,
    pub(crate) draws: u32,
    pub(crate) render_passes: u32,
    pub(crate) compute_passes: u32,
    pub(crate) command_buffers: u32,
    pub(crate) pipeline_builds: u32,
    pub(crate) textures_created: u32,
}

/// What one capture delivers.
#[derive(Debug)]
pub(crate) struct Captured {
    pub(crate) want: Want,
    pub(crate) frame: u64,
    pub(crate) info: FrameInfo,
    pub(crate) passes: Vec<PassRec>,
    pub(crate) images: Vec<Image>,
}

/// The name of a file stem for an image: `final`, `pass-3-c0`, `pass-3-depth` or `texture-17`.
pub(crate) fn stem(key: &Key) -> String {
    match key {
        Key::Final => "final".to_owned(),
        Key::Pass(index, label) => format!("pass-{index}-{label}"),
        Key::Texture(id) => format!("texture-{id}"),
    }
}

/// The names of a list of keys for a line of text: the first `max`, then the count of the rest.
pub(crate) fn short_list(keys: &[String], max: usize) -> String {
    if keys.is_empty() {
        return "-".to_owned();
    }
    let shown = keys.iter().take(max).cloned().collect::<Vec<_>>().join(",");
    match keys.len().saturating_sub(max) {
        0 => shown,
        more => format!("{shown},+{more}"),
    }
}

impl PassRec {
    /// The line of the `passes` verb: index, kind, attachments, draws and programs.
    pub(crate) fn line(&self) -> String {
        let mut out = format!("pass {} {}", self.index, self.kind);
        for a in &self.attachments {
            out.push_str(&format!(
                " {}:t{}:{}:{}x{}:{}>{}",
                a.label, a.tex, a.format, a.width, a.height, a.load, a.store
            ));
        }
        if self.kind == "Render" {
            out.push_str(&format!(
                " draws {} vs {} fs {}",
                self.draws,
                short_list(&self.vertex, 3),
                short_list(&self.fragment, 3)
            ));
            if !self.mesh.is_empty() {
                out.push_str(&format!(" ms {}", short_list(&self.mesh, 3)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_name_the_files() {
        assert_eq!(stem(&Key::Final), "final");
        assert_eq!(stem(&Key::Pass(12, "c1".into())), "pass-12-c1");
        assert_eq!(stem(&Key::Pass(3, "depth".into())), "pass-3-depth");
        assert_eq!(stem(&Key::Texture(77)), "texture-77");
    }

    #[test]
    fn long_lists_are_cut() {
        let keys: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        assert_eq!(short_list(&keys, 3), "a,b,c,+2");
        assert_eq!(short_list(&keys[..2], 3), "a,b");
        assert_eq!(short_list(&[], 3), "-");
    }

    #[test]
    fn a_render_pass_prints_one_line() {
        let p = PassRec {
            index: 4,
            kind: "Render".into(),
            draws: 12,
            attachments: vec![AttRec {
                label: "c0".into(),
                tex: 9,
                format: "Rgba16Float".into(),
                width: 1920,
                height: 1080,
                load: "Clear".into(),
                store: "Store".into(),
                clear: Some(vec![0.0; 4]),
                skipped: None,
            }],
            vertex: vec!["v1".into()],
            fragment: vec!["f1".into(), "f2".into()],
            ..PassRec::default()
        };
        assert_eq!(
            p.line(),
            "pass 4 Render c0:t9:Rgba16Float:1920x1080:Clear>Store draws 12 vs v1 fs f1,f2"
        );
        let c = PassRec {
            index: 5,
            kind: "Compute".into(),
            ..PassRec::default()
        };
        assert_eq!(c.line(), "pass 5 Compute");
    }
}
