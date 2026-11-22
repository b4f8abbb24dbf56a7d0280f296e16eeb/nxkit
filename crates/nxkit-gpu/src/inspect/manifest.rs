//! The `manifest.json` of a capture.
//!
//! Schema 1. The top level holds `schema`, `frame`, `mode` (`frame`, `passes` or `texture`), `encode` (the numbers of
//! the frame), `final` (an image entry or null), `passes` (every pass of the plan), `textures` (the image entries of a
//! texture capture) and `files` (every file of the folder). An image entry holds `tex`, `format`, `width`, `height`
//! and either `file`, `note`, `stats` and, for float and depth formats, `raw` and `raw_layout`, or `skipped` with the
//! reason. A pass holds `index`, `kind`, `cmds`, `draws`, `programs`, `blend`, `gpu_ns` and `attachments`, and an
//! attachment holds `slot`, `tex`, `format`, `width`, `height`, `load`, `store`, `clear` and the fields of an image
//! entry.

use super::Want;
use super::convert::{RawLayout, Stats};
use super::data::{AttRec, BlendRec, Captured, FrameInfo, Image, Key, PassRec};
use super::jsonw::J;

/// What the writer made of one image.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileRec {
    pub(crate) key: Key,
    pub(crate) file: Option<String>,
    pub(crate) raw: Option<String>,
    pub(crate) layout: Option<RawLayout>,
    pub(crate) stats: Option<Stats>,
    pub(crate) note: Option<&'static str>,
    pub(crate) skipped: Option<String>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

fn floats(v: &[f32]) -> J {
    J::Arr(v.iter().map(|x| J::num32(*x)).collect())
}

fn stats(s: &Stats) -> J {
    let n = usize::from(s.channels);
    J::obj([
        ("channels", J::uint(s.channels)),
        ("min", floats(&s.min[..n])),
        ("max", floats(&s.max[..n])),
        ("nan", J::uint(s.nan)),
        ("inf", J::uint(s.inf)),
        ("over1", J::uint(s.over1)),
    ])
}

fn layout(l: &RawLayout, width: u32) -> J {
    J::obj([
        ("bytes_per_pixel", J::uint(l.bytes_per_pixel)),
        (
            "row_bytes",
            J::uint(u64::from(width) * u64::from(l.bytes_per_pixel)),
        ),
        ("encoding", J::str(l.encoding)),
        (
            "order",
            J::str("little endian, rows from the top, no padding"),
        ),
    ])
}

/// The fields of an image entry that the writer made.
fn file_fields(f: Option<&FileRec>) -> Vec<(String, J)> {
    let Some(f) = f else {
        return vec![("file".into(), J::Null)];
    };
    if let Some(why) = &f.skipped {
        return vec![("skipped".into(), J::str(why.as_str()))];
    }
    let mut out = vec![
        ("file".to_owned(), J::opt_str(f.file.as_deref())),
        ("note".to_owned(), J::opt_str(f.note)),
    ];
    if let Some(s) = &f.stats {
        out.push(("stats".into(), stats(s)));
    }
    if let Some(r) = &f.raw {
        out.push(("raw".into(), J::str(r.as_str())));
    }
    if let Some(l) = &f.layout {
        out.push(("raw_layout".into(), layout(l, f.width)));
    }
    out
}

fn image_entry(img: &Image, f: Option<&FileRec>) -> J {
    let mut o = vec![
        ("tex".to_owned(), img.tex.map_or(J::Null, J::uint)),
        ("format".to_owned(), J::str(format!("{:?}", img.format))),
        ("width".to_owned(), J::uint(img.width)),
        ("height".to_owned(), J::uint(img.height)),
    ];
    o.extend(file_fields(f));
    J::Obj(o)
}

fn blend(b: &BlendRec) -> J {
    J::obj([
        ("enable", J::Bool(b.enable)),
        ("src_rgb", J::str(b.src_rgb.as_str())),
        ("dst_rgb", J::str(b.dst_rgb.as_str())),
        ("op_rgb", J::str(b.op_rgb.as_str())),
        ("src_alpha", J::str(b.src_alpha.as_str())),
        ("dst_alpha", J::str(b.dst_alpha.as_str())),
        ("op_alpha", J::str(b.op_alpha.as_str())),
        ("write_mask", J::uint(b.write_mask)),
    ])
}

fn attachment(a: &AttRec, pass: usize, files: &[FileRec]) -> J {
    let found = files
        .iter()
        .find(|f| f.key == Key::Pass(pass, a.label.clone()));
    let mut o = vec![
        ("slot".to_owned(), J::str(a.label.as_str())),
        ("tex".to_owned(), J::uint(a.tex)),
        ("format".to_owned(), J::str(a.format.as_str())),
        ("width".to_owned(), J::uint(a.width)),
        ("height".to_owned(), J::uint(a.height)),
        ("load".to_owned(), J::str(a.load.as_str())),
        ("store".to_owned(), J::str(a.store.as_str())),
        (
            "clear".to_owned(),
            a.clear
                .as_ref()
                .map_or(J::Null, |c| J::Arr(c.iter().map(|x| J::num(*x)).collect())),
        ),
    ];
    match (found, &a.skipped) {
        (None, Some(why)) => o.push(("skipped".into(), J::str(why.as_str()))),
        (f, _) => o.extend(file_fields(f)),
    }
    J::Obj(o)
}

fn pass(p: &PassRec, files: &[FileRec]) -> J {
    J::obj([
        ("index", J::uint(p.index)),
        ("kind", J::str(p.kind.as_str())),
        ("cmds", J::Arr(p.cmds.iter().map(|c| J::uint(*c)).collect())),
        ("draws", J::uint(p.draws)),
        (
            "programs",
            J::obj([
                ("vertex", J::strs(&p.vertex)),
                ("fragment", J::strs(&p.fragment)),
                ("task", J::strs(&p.task)),
                ("mesh", J::strs(&p.mesh)),
            ]),
        ),
        (
            "blend",
            J::Arr(
                p.blend
                    .iter()
                    .map(|(slot, states)| {
                        J::obj([
                            ("slot", J::uint(*slot)),
                            ("states", J::Arr(states.iter().map(blend).collect())),
                        ])
                    })
                    .collect(),
            ),
        ),
        // The GPU time of one pass is not measured by the capture.
        ("gpu_ns", J::Null),
        (
            "attachments",
            J::Arr(
                p.attachments
                    .iter()
                    .map(|a| attachment(a, p.index, files))
                    .collect(),
            ),
        ),
    ])
}

fn encode(i: &FrameInfo) -> J {
    J::obj([
        ("encode_ns", J::uint(i.encode_ns)),
        ("gpu_ns", i.gpu_ns.map_or(J::Null, J::uint)),
        ("draws", J::uint(i.draws)),
        ("render_passes", J::uint(i.render_passes)),
        ("compute_passes", J::uint(i.compute_passes)),
        ("command_buffers", J::uint(i.command_buffers)),
        ("pipeline_builds", J::uint(i.pipeline_builds)),
        ("textures_created", J::uint(i.textures_created)),
    ])
}

/// The name of a mode in the manifest.
pub(crate) fn mode(w: &Want) -> &'static str {
    match w {
        Want::Frame => "frame",
        Want::Passes => "passes",
        Want::List => "list",
        Want::Texture(_) => "texture",
    }
}

/// The manifest of a capture. `files` is what the writer made of each image, `names` every file of the folder.
pub(crate) fn build(c: &Captured, files: &[FileRec], names: &[String]) -> J {
    let rec = |key: &Key| files.iter().find(|f| &f.key == key);
    let fin = c.images.iter().find(|i| i.key == Key::Final);
    let textures: Vec<J> = c
        .images
        .iter()
        .filter(|i| matches!(i.key, Key::Texture(_)))
        .map(|i| image_entry(i, rec(&i.key)))
        .collect();
    let mut o = vec![
        ("schema", J::Int(1)),
        ("frame", J::uint(c.frame)),
        ("mode", J::str(mode(&c.want))),
        ("encode", encode(&c.info)),
        (
            "final",
            fin.map_or(J::Null, |i| image_entry(i, rec(&Key::Final))),
        ),
        (
            "passes",
            J::Arr(c.passes.iter().map(|p| pass(p, files)).collect()),
        ),
    ];
    if !textures.is_empty() {
        o.push(("textures", J::Arr(textures)));
    }
    o.push(("files", J::strs(names)));
    J::Obj(o.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{Json, parse};
    use nxkit_metal::Format;

    fn fixture() -> (Captured, Vec<FileRec>) {
        let p = PassRec {
            index: 1,
            kind: "Render".into(),
            cmds: [3, 9],
            draws: 2,
            attachments: vec![
                AttRec {
                    label: "c0".into(),
                    tex: 5,
                    format: "Rgba16Float".into(),
                    width: 4,
                    height: 2,
                    load: "Clear".into(),
                    store: "Store".into(),
                    clear: Some(vec![0.0, 0.0, 0.0, 1.0]),
                    skipped: None,
                },
                AttRec {
                    label: "depth".into(),
                    tex: 6,
                    format: "Depth32FloatStencil8".into(),
                    width: 4,
                    height: 2,
                    load: "Clear".into(),
                    store: "DontCare".into(),
                    clear: Some(vec![1.0]),
                    skipped: Some("the store action is DontCare".into()),
                },
            ],
            vertex: vec!["v1".into()],
            fragment: vec!["f1".into()],
            blend: vec![(
                0,
                vec![BlendRec {
                    enable: true,
                    src_rgb: "SourceAlpha".into(),
                    dst_rgb: "OneMinusSourceAlpha".into(),
                    op_rgb: "Add".into(),
                    src_alpha: "One".into(),
                    dst_alpha: "Zero".into(),
                    op_alpha: "Add".into(),
                    write_mask: 15,
                }],
            )],
            ..PassRec::default()
        };
        let c = Captured {
            want: Want::Passes,
            frame: 42,
            info: FrameInfo {
                encode_ns: 1_000,
                gpu_ns: Some(2_000),
                draws: 2,
                ..FrameInfo::default()
            },
            passes: vec![
                PassRec {
                    index: 0,
                    kind: "Compute".into(),
                    ..PassRec::default()
                },
                p,
            ],
            images: vec![Image {
                key: Key::Final,
                tex: Some(5),
                format: Format::Bgra8Unorm,
                width: 4,
                height: 2,
                data: Ok(vec![]),
            }],
        };
        let files = vec![
            FileRec {
                key: Key::Final,
                file: Some("final.png".into()),
                raw: None,
                layout: None,
                stats: None,
                note: Some("blue and red swapped"),
                skipped: None,
                width: 4,
                height: 2,
            },
            FileRec {
                key: Key::Pass(1, "c0".into()),
                file: Some("pass-1-c0.png".into()),
                raw: Some("pass-1-c0.raw".into()),
                layout: Some(RawLayout {
                    bytes_per_pixel: 8,
                    encoding: "rgba16f",
                }),
                stats: Some(Stats {
                    channels: 4,
                    min: [0.0, 0.0, 0.0, 1.0],
                    max: [3.5, 1.0, 0.0, 1.0],
                    nan: 0,
                    inf: 0,
                    over1: 1,
                }),
                note: Some("linear values clamped to 0..1, no gamma"),
                skipped: None,
                width: 4,
                height: 2,
            },
        ];
        (c, files)
    }

    #[test]
    fn the_manifest_has_the_documented_shape() {
        let (c, files) = fixture();
        let names = vec!["final.png".to_owned(), "manifest.json".to_owned()];
        let text = build(&c, &files, &names).pretty();
        let m = parse(&text).expect("the manifest is valid json");
        assert_eq!(m.get("schema").and_then(Json::as_i64), Some(1));
        assert_eq!(m.get("frame").and_then(Json::as_i64), Some(42));
        assert_eq!(m.get("mode").and_then(Json::as_str), Some("passes"));
        let fin = m.get("final").unwrap();
        assert_eq!(fin.get("file").and_then(Json::as_str), Some("final.png"));
        assert_eq!(fin.get("format").and_then(Json::as_str), Some("Bgra8Unorm"));
        let encode = m.get("encode").unwrap();
        assert_eq!(encode.get("gpu_ns").and_then(Json::as_i64), Some(2000));
        let Some(Json::Arr(passes)) = m.get("passes") else {
            panic!("passes is a list");
        };
        assert_eq!(passes.len(), 2);
        assert_eq!(
            passes[0].get("kind").and_then(Json::as_str),
            Some("Compute")
        );
        let render = &passes[1];
        assert_eq!(render.get("draws").and_then(Json::as_i64), Some(2));
        assert_eq!(render.get("gpu_ns"), Some(&Json::Null));
        let progs = render.get("programs").unwrap();
        assert!(matches!(progs.get("vertex"), Some(Json::Arr(v)) if v.len() == 1));
        let Some(Json::Arr(atts)) = render.get("attachments") else {
            panic!("attachments is a list");
        };
        let c0 = &atts[0];
        assert_eq!(c0.get("file").and_then(Json::as_str), Some("pass-1-c0.png"));
        assert_eq!(c0.get("raw").and_then(Json::as_str), Some("pass-1-c0.raw"));
        let layout = c0.get("raw_layout").unwrap();
        assert_eq!(layout.get("row_bytes").and_then(Json::as_i64), Some(32));
        assert_eq!(
            layout.get("encoding").and_then(Json::as_str),
            Some("rgba16f")
        );
        let st = c0.get("stats").unwrap();
        assert_eq!(st.get("over1").and_then(Json::as_i64), Some(1));
        assert!(matches!(st.get("max"), Some(Json::Arr(v)) if v[0] == Json::Num(3.5)));
        let depth = &atts[1];
        assert_eq!(
            depth.get("skipped").and_then(Json::as_str),
            Some("the store action is DontCare")
        );
        assert!(depth.get("file").is_none());
        let Some(Json::Arr(blend)) = render.get("blend") else {
            panic!("blend is a list");
        };
        let state = match blend[0].get("states") {
            Some(Json::Arr(s)) => &s[0],
            _ => panic!("states"),
        };
        assert_eq!(
            state.get("src_rgb").and_then(Json::as_str),
            Some("SourceAlpha")
        );
        assert_eq!(state.get("write_mask").and_then(Json::as_i64), Some(15));
    }

    #[test]
    fn a_skipped_image_has_a_reason_and_no_file() {
        let (c, mut files) = fixture();
        files.truncate(1);
        files[0].skipped = Some("no conversion".into());
        files[0].file = None;
        let m = parse(&build(&c, &files, &[]).pretty()).unwrap();
        let fin = m.get("final").unwrap();
        assert_eq!(
            fin.get("skipped").and_then(Json::as_str),
            Some("no conversion")
        );
        assert!(fin.get("file").is_none());
    }

    #[test]
    fn a_frame_capture_without_pictures_still_lists_the_attachments() {
        let (c, _) = fixture();
        let m = parse(&build(&c, &[], &[]).pretty()).unwrap();
        let Some(Json::Arr(passes)) = m.get("passes") else {
            panic!()
        };
        let Some(Json::Arr(atts)) = passes[1].get("attachments") else {
            panic!()
        };
        assert_eq!(atts[0].get("file"), Some(&Json::Null));
    }

    #[test]
    fn modes_have_names() {
        assert_eq!(mode(&Want::Frame), "frame");
        assert_eq!(mode(&Want::Passes), "passes");
        assert_eq!(mode(&Want::Texture(3)), "texture");
        assert_eq!(mode(&Want::List), "list");
    }
}
