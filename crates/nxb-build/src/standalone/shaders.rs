//! The shader part of the standalone build: the profile folder the split and the shader step read, the shader step
//! with the pipeline archive (`crate::pipeline`), and the files copied into the run folder.

use crate::Args;
use crate::events::{Progress, Update};
use crate::graph::Build;
use nxtitle::Standalone;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn io(p: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", p.display())
}

/// The profile folder `<work>/profile`: the `game.tsv` the shader split reads (what the title module says about the
/// game's shaders) and the hand-written shader facts. The split's own tables join it after the extract
/// ([`take_tables`]). Returns the folder and the `game.tsv` keys.
pub(crate) fn profile(
    work: &Path,
    st: &Standalone,
    title_id: u64,
    name: &str,
) -> Result<(PathBuf, BTreeMap<String, String>), String> {
    let dir = work.join("profile");
    std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
    let package = format!("v{}", st.version);
    let rows = [
        ("title_id", format!("{title_id:016X}")),
        ("name", name.to_string()),
        ("package", package.clone()),
        ("shader_versions", package),
        ("shader_groups", st.shader_groups.to_string()),
        ("shader_note", st.shader_note.to_string()),
    ];
    // the split reads a compressed file outside the groups as it is (see `Profile::reads`)
    let scope = st
        .shader_groups_only
        .then(|| ("shader_scope", "groups".to_string()));
    let rows: Vec<_> = rows.into_iter().chain(scope).collect();
    let mut text = String::from(
        "# Written by nxb build from the title module. Edit the title module, not this file.\n",
    );
    for (k, v) in &rows {
        text.push_str(&format!("{k}\t{v}\n"));
    }
    std::fs::write(dir.join("game.tsv"), text).map_err(|e| io(&dir, e))?;
    let manual = dir.join("shader_facts_manual.tsv");
    match st.shader_facts {
        Some(t) => std::fs::write(&manual, t).map_err(|e| io(&manual, e))?,
        None => {
            let _ = std::fs::remove_file(&manual);
        }
    }
    Ok((
        dir,
        rows.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    ))
}

/// The split's tables go into the profile folder, where the shader step reads them.
pub(crate) fn take_tables(work: &Path, profile: &Path) -> Result<(), String> {
    for n in [
        "shader_map.tsv",
        "shader_facts.tsv",
        "shader_pairs.tsv",
        "pso_pairs.tsv",
    ] {
        let src = work.join("shader").join(n);
        if src.is_file() {
            std::fs::copy(&src, profile.join(n)).map_err(|e| io(&src, e))?;
        }
    }
    Ok(())
}

/// What the stage left in the run folder.
pub(crate) struct Done {
    /// The shader step's and the archive's summary.
    pub summary: String,
}

/// Runs the shader step and the archive, then copies the Metal libraries, their `_b.json` reflection and the tables
/// into `<run>/shaders` and the archive to `<run>/pipelines/` (the shards and their index) or `<run>/pipelines.mtl4archive`
/// (`NXB_ARCHIVE_LAYOUT`, see `crate::archive_layout`).
pub(crate) fn run(a: &Args, b: &Build, p: &Arc<Progress>, run: &Path) -> Result<Done, String> {
    run_for_target(a, b, p, run, false)
}

/// Run the existing title shader pipeline for Mac or iOS.
pub(crate) fn run_for_target(
    a: &Args,
    b: &Build,
    p: &Arc<Progress>,
    run: &Path,
    ios: bool,
) -> Result<Done, String> {
    let layout = crate::archive_layout::Layout::from_env();
    let o = crate::pipeline::shaders_and_archive_for_target(a, b, p, layout, ios)?;
    let dst = run.join("shaders");
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).map_err(|e| io(&dst, e))?;
    let keep: Vec<_> = std::fs::read_dir(&o.lib)
        .map_err(|e| io(&o.lib, e))?
        .flatten()
        .filter(|e| {
            let path = e.path();
            path.extension()
                .is_some_and(|x| x == "metallib" || x == "json" || x == "tsv")
                && path.is_file()
        })
        .collect();
    let n = keep.len();
    p.set(Update {
        total: Some(n as u64),
        done: Some(0),
        unit: Some("files"),
        note: Some("copying shader libraries"),
        ..Default::default()
    });
    let count = crate::events::Count::default();
    count.set_total(n as u64);
    p.counted(None, "files", &count, || {
        for e in &keep {
            let path = e.path();
            std::fs::copy(&path, dst.join(e.file_name())).map_err(|x| io(&path, x))?;
            count.add(1);
        }
        Ok::<(), String>(())
    })?;
    if n == 0 {
        return Err(format!(
            "the shader step left no library in {}",
            o.lib.display()
        ));
    }
    let mut summary = format!("{n} shader files ({})", o.shaders);
    match (b.gpus.first(), &o.archive_error) {
        (Some(arch), None) => {
            p.set(Update {
                note: Some("copying the pipeline archive"),
                ..Default::default()
            });
            if layout.merges() {
                let src = crate::pipeline::archive_dir(b, ios).join(format!("{arch}.mtl4a"));
                if !src.is_file() {
                    return Err(format!(
                        "the archive step reported no error and left no file at {}",
                        src.display()
                    ));
                }
                std::fs::copy(&src, run.join("pipelines.mtl4archive")).map_err(|e| io(&src, e))?;
            }
            if layout.shards() {
                let n = crate::archive_layout::copy_to_run(
                    &crate::pipeline::archive_dir(b, ios).join(nxkit_abi::shards::SHARD_DIR),
                    run,
                )?;
                summary.push_str(&format!(", {n} archive shards"));
            }
            if let Some(s) = &o.archive {
                summary.push_str(&format!(", archive: {s}"));
            }
        }
        (Some(_), Some(why)) => {
            p.say(&format!(
                "nxb: warning: no pipeline archive: {why}. The app is built without pipelines.mtl4archive and compiles its pipelines while the game runs."
            ));
        }
        (None, _) => summary.push_str(", no archive (--gpu none)"),
    }
    Ok(Done { summary })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_has_the_keys_the_split_reads() {
        let d = std::env::temp_dir().join(format!("nxb-standalone-prof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let st = Standalone {
            version: 7,
            modules: &[],
            nvn_functions: nxtitle::Fact::new("", nxtitle::Evidence::Unverified { note: "test" }),
            shader_groups: "/Shader/=shader",
            shader_groups_only: true,
            shader_note: "note",
            shader_facts: Some("k\t{}\n"),
            romfs_marker: "/x",
            settings: nxtitle::Fact::new(
                nxtitle::Settings::DEFAULT,
                nxtitle::Evidence::Unverified { note: "test" },
            ),
        };
        let (dir, keys) = profile(&d, &st, 0x0100_0000_0000_0000, "Game").unwrap();
        let text = std::fs::read_to_string(dir.join("game.tsv")).unwrap();
        assert!(
            text.contains("shader_versions\tv7\n") && text.contains("title_id\t0100000000000000\n")
        );
        assert!(text.contains("shader_scope\tgroups\n"));
        assert_eq!(
            keys.get("shader_groups").map(String::as_str),
            Some("/Shader/=shader")
        );
        assert!(dir.join("shader_facts_manual.tsv").is_file());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
