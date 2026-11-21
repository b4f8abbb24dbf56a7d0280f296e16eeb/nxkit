//! The stand-ins for the console's shared fonts in `Contents/Resources/nxkit/font` (`nxb-font`). `nxkit-svc-system`
//! reads them from there: without `nintendo_ext.ttf` a game draws its button icons as "?", and a game whose font
//! engine is sized for the console's fonts cannot load macOS's Arial Unicode as it is. The fonts are made from the
//! macOS system fonts at build time, so no font file is in the repository or in the release.

use std::path::Path;

/// Makes the six fonts into `dir` and returns a summary line.
pub(crate) fn write(dir: &Path) -> Result<String, String> {
    let read = |p: &str| std::fs::read(p).map_err(|e| format!("{p}: {e}"));
    let fonts = nxb_font::shared_fonts(&read(nxb_font::SOURCE)?, &read(nxb_font::LETTERS)?)
        .map_err(|e| format!("shared fonts: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut bytes = 0;
    for f in &fonts {
        let to = dir.join(f.file);
        std::fs::write(&to, &f.bytes).map_err(|e| format!("{}: {e}", to.display()))?;
        bytes += f.bytes.len();
    }
    Ok(format!(
        "{} shared fonts, {:.1} MiB, in {}",
        fonts.len(),
        bytes as f64 / (1024.0 * 1024.0),
        dir.display()
    ))
}

/// The font folder of `target`: `Contents/Resources/nxkit/font` of an app of nxkit, else `target` itself.
pub(crate) fn folder(target: &Path) -> std::path::PathBuf {
    let run = target.join("Contents/Resources/nxkit");
    if run.is_dir() {
        run.join("font")
    } else {
        target.to_path_buf()
    }
}
