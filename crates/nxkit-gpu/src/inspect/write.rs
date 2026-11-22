//! Writes a capture into its folder: the pictures, the raw bytes of the float formats and `manifest.json`.

use std::path::{Path, PathBuf};

use super::convert::convert;
use super::data::{Captured, Image, Key, stem};
use super::manifest::{self, FileRec};
use super::{Want, png};

/// The folder of the next capture inside `base`: `capture-<n>` with the smallest `n` that does not exist yet.
pub(crate) fn new_folder(base: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(base).map_err(|e| format!("{}: {e}", base.display()))?;
    for n in 1..100_000u32 {
        let dir = base.join(format!("capture-{n}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        }
    }
    Err(format!("{}: no free capture folder", base.display()))
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Converts one image and writes its files. A format that cannot be converted gives a record with `skipped`.
pub(crate) fn write_image(dir: &Path, img: &Image) -> Result<FileRec, String> {
    let stem = stem(&img.key);
    let mut rec = FileRec {
        key: img.key.clone(),
        file: None,
        raw: None,
        layout: None,
        stats: None,
        note: None,
        skipped: None,
        width: img.width,
        height: img.height,
    };
    let bytes = match &img.data {
        Ok(b) => b,
        Err(why) => {
            rec.skipped = Some(why.clone());
            return Ok(rec);
        }
    };
    let converted = match convert(img.format, img.width, img.height, bytes) {
        Ok(c) => c,
        Err(why) => {
            rec.skipped = Some(why);
            return Ok(rec);
        }
    };
    let Some(file) = png::encode_rgba(img.width, img.height, &converted.rgba) else {
        rec.skipped = Some("the picture has no size".to_owned());
        return Ok(rec);
    };
    put(dir, &format!("{stem}.png"), &file)?;
    rec.file = Some(format!("{stem}.png"));
    if converted.raw.is_some() {
        put(dir, &format!("{stem}.raw"), bytes)?;
        rec.raw = Some(format!("{stem}.raw"));
    }
    rec.layout = converted.raw;
    rec.stats = Some(converted.stats);
    rec.note = Some(converted.note);
    Ok(rec)
}

/// What a written capture tells the caller.
#[derive(Debug)]
pub(crate) struct Written {
    pub(crate) dir: PathBuf,
    pub(crate) lines: Vec<String>,
}

/// Writes the capture into `dir` (which exists) and gives the summary lines.
pub(crate) fn write_into(dir: &Path, c: &Captured) -> Result<Written, String> {
    let mut files = Vec::new();
    for img in &c.images {
        files.push(write_image(dir, img)?);
    }
    let mut names: Vec<String> = files
        .iter()
        .flat_map(|f| f.file.iter().chain(f.raw.iter()).cloned())
        .collect();
    names.push("manifest.json".to_owned());
    names.sort();
    let json = manifest::build(c, &files, &names).pretty();
    put(dir, "manifest.json", json.as_bytes())?;
    Ok(Written {
        dir: dir.to_path_buf(),
        lines: summary(c, &files),
    })
}

/// Writes a capture into a new folder under `base`.
pub(crate) fn write(base: &Path, c: &Captured) -> Result<Written, String> {
    let dir = new_folder(base)?;
    write_into(&dir, c)
}

/// The summary: what the frame had and how many pictures were written or skipped.
pub(crate) fn summary(c: &Captured, files: &[FileRec]) -> Vec<String> {
    let written = files.iter().filter(|f| f.file.is_some()).count();
    let skipped: Vec<&FileRec> = files.iter().filter(|f| f.skipped.is_some()).collect();
    let render = c.passes.iter().filter(|p| p.kind == "Render").count();
    let mut out = Vec::new();
    match &c.want {
        Want::Texture(id) => out.push(format!("frame {}: texture {id}", c.frame)),
        _ => out.push(format!(
            "frame {}: {} passes ({render} render), {} draws",
            c.frame,
            c.passes.len(),
            c.info.draws
        )),
    }
    out.push(format!(
        "{written} pictures written, {} skipped, mode {}",
        skipped.len(),
        manifest::mode(&c.want)
    ));
    for f in skipped.iter().take(10) {
        out.push(format!(
            "skipped {}: {}",
            stem(&f.key),
            f.skipped.as_deref().unwrap_or_default()
        ));
    }
    if skipped.len() > 10 {
        out.push(format!(
            "and {} more skipped (see manifest.json)",
            skipped.len() - 10
        ));
    }
    if let Some(f) = files.iter().find(|f| f.key == Key::Final)
        && let Some(file) = &f.file
    {
        out.push(format!("final {file}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::data::{AttRec, FrameInfo, PassRec};
    use nxkit_metal::Format;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nxkit-inspect-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn half_bytes(vals: &[f32]) -> Vec<u8> {
        vals.iter()
            .flat_map(|v| {
                let bits: u16 = match *v {
                    1.0 => 0x3c00,
                    2.0 => 0x4000,
                    _ => 0,
                };
                bits.to_le_bytes()
            })
            .collect()
    }

    fn capture() -> Captured {
        Captured {
            want: Want::Passes,
            frame: 7,
            info: FrameInfo::default(),
            passes: vec![PassRec {
                index: 0,
                kind: "Render".into(),
                attachments: vec![
                    AttRec {
                        label: "c0".into(),
                        tex: 1,
                        format: "Rgba16Float".into(),
                        width: 2,
                        height: 1,
                        load: "Clear".into(),
                        store: "Store".into(),
                        clear: None,
                        skipped: None,
                    },
                    AttRec {
                        label: "depth".into(),
                        tex: 2,
                        format: "Stencil8".into(),
                        width: 2,
                        height: 1,
                        load: "Clear".into(),
                        store: "Store".into(),
                        clear: None,
                        skipped: None,
                    },
                ],
                ..PassRec::default()
            }],
            images: vec![
                Image {
                    key: Key::Final,
                    tex: Some(1),
                    format: Format::Rgba8Unorm,
                    width: 2,
                    height: 1,
                    data: Ok(vec![1, 2, 3, 4, 5, 6, 7, 8]),
                },
                Image {
                    key: Key::Pass(0, "c0".into()),
                    tex: Some(1),
                    format: Format::Rgba16Float,
                    width: 2,
                    height: 1,
                    data: Ok(half_bytes(&[2.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0])),
                },
                Image {
                    key: Key::Pass(0, "depth".into()),
                    tex: Some(2),
                    format: Format::Stencil8,
                    width: 2,
                    height: 1,
                    data: Ok(vec![0, 0]),
                },
                Image {
                    key: Key::Texture(9),
                    tex: Some(9),
                    format: Format::Rgba8Unorm,
                    width: 1,
                    height: 1,
                    data: Err("the texture has more than one sample".into()),
                },
            ],
        }
    }

    #[test]
    fn folders_count_up_and_never_reuse_a_name() {
        let base = temp("folders");
        let a = new_folder(&base).unwrap();
        let b = new_folder(&base).unwrap();
        assert_eq!(a.file_name().unwrap(), "capture-1");
        assert_eq!(b.file_name().unwrap(), "capture-2");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_capture_writes_pictures_raw_bytes_and_the_manifest() {
        let base = temp("write");
        let w = write(&base, &capture()).unwrap();
        let ls = |n: &str| w.dir.join(n);
        // The final picture is a real PNG with the swatch of the bytes.
        let final_png = std::fs::read(ls("final.png")).unwrap();
        assert_eq!(&final_png[1..4], b"PNG");
        assert!(ls("pass-0-c0.png").exists());
        // Float formats keep their bytes beside the picture. An 8-bit format does not.
        assert_eq!(std::fs::read(ls("pass-0-c0.raw")).unwrap().len(), 16);
        assert!(!ls("final.raw").exists());
        // The formats without a conversion and the failed reads leave no file and a reason in the manifest.
        assert!(!ls("pass-0-depth.png").exists());
        assert!(!ls("texture-9.png").exists());
        let text = std::fs::read_to_string(ls("manifest.json")).unwrap();
        let m = crate::json::parse(&text).expect("valid json");
        assert!(text.contains("\"skipped\": \"the format Stencil8 has no conversion"));
        assert!(text.contains("more than one sample"));
        assert!(text.contains("\"pass-0-c0.raw\""));
        assert!(m.get("files").is_some());
        assert!(w.lines[0].starts_with("frame 7: 1 passes (1 render)"));
        assert!(w.lines.iter().any(|l| l == "final final.png"));
        assert!(
            w.lines
                .iter()
                .any(|l| l.starts_with("2 pictures written, 2 skipped"))
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
