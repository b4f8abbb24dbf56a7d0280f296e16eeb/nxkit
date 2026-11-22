//! The pipeline list that the app ships (`nxkit_pipestate::list::SHIPPED_LIST`): the seeds of the title module resolved
//! against the shaders of this build (`nxb_archive::seeded`). The host makes these pipelines at every start before the
//! first frame, from the archive that holds them. The entries have no keys: the host computes them with its key scheme.

use std::path::Path;

use nxb_archive::seeded::Resolver;
use nxkit_pipestate::list::{SHIPPED_LIST, Seen};
use nxkit_pipestate::seeds::Seeds;

/// Writes the list into the run folder, or removes an old one when the title has no seeds. Returns the note for the
/// summary, empty without seeds.
pub(crate) fn write(seeds: &Seeds, lib: &Path, run: &Path) -> Result<String, String> {
    let path = run.join(SHIPPED_LIST);
    if seeds.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(String::new());
    }
    let (entries, n) = Resolver::new(lib).resolve(seeds, |_| true);
    let mut list = Seen::default();
    for e in entries {
        list.insert(e);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, list.to_text()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!(
        ", {} shipped pipelines from {} seeds ({} name shaders this build lacks, {} do not fit its shaders)",
        list.len(),
        seeds.len(),
        n.no_shader,
        n.refused
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_written_and_removed_with_the_seeds() {
        let d = std::env::temp_dir().join(format!("nxb-shipped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (lib, run) = (d.join("lib"), d.join("run"));
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("00000000000000a1_b.json"), r#"{"entry": "kern"}"#).unwrap();
        let seeds = Seeds::parse("nxkit-pipeline-seeds 1\nC\t00000000000000a1\n").unwrap();
        let note = write(&seeds, &lib, &run).unwrap();
        assert!(note.contains("1 shipped pipelines"), "{note}");
        let text = std::fs::read_to_string(run.join(SHIPPED_LIST)).unwrap();
        assert_eq!(Seen::parse(&text).unwrap().len(), 1);
        assert!(text.contains("C\t-\t00000000000000a1_b\tkern"));
        assert_eq!(write(&Seeds::default(), &lib, &run).unwrap(), "");
        assert!(!run.join(SHIPPED_LIST).exists());
    }
}
