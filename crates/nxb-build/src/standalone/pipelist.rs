//! The lists of pipelines that the host recorded, which the archive step compiles for the new app.
//!
//! A title that was played has `pipelines.seen` (`nxkit-gpu`, `pipelines/record.rs`): the descriptions of the pipelines
//! it asked for. They do not depend on the shader binaries, so the list of the old app is good for the new one. The
//! build looks for it where the old app (the build replaces it at the output path) and the host leave it:
//!
//! - `<app>/Contents/Resources/nxkit/pipecache/` of the app at the output path,
//! - `~/Library/Caches/nxkit/<title id>/pipecache/` (the cache folder when the app cannot be written),
//! - `$NXKIT_PIPELINE_CACHE_DIR/pipecache/`,
//!
//! and in each folder `seen-*.list` (the list that survives rebuilds) and `id-*/pipelines.seen` (the list of the
//! earlier layout). `--pipeline-list FILE` names a list and replaces the search. Lists are merged without repeats by
//! the archive step.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The list files in the cache folder `<root>/pipecache`, newest first.
pub(crate) fn lists_in(root: &Path) -> Vec<PathBuf> {
    let base = root.join("pipecache");
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut add = |p: PathBuf| {
        if let Ok(t) = p.metadata().and_then(|m| m.modified()) {
            found.push((t, p));
        }
    };
    for e in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("seen-") && name.ends_with(".list") {
            add(e.path());
        } else if name.starts_with("id-") {
            add(e.path().join("pipelines.seen"));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|f| f.1).collect()
}

/// The lists for a build: `--pipeline-list`, or those of the old app at `out`, of the cache folder of the title in
/// the user's library and of `NXKIT_PIPELINE_CACHE_DIR`. `home` is the user's home folder.
pub(crate) fn find(
    given: Option<&Path>,
    out: &Path,
    title: &str,
    home: Option<&Path>,
    env_dir: Option<&Path>,
) -> Vec<PathBuf> {
    if let Some(g) = given {
        return vec![g.to_path_buf()];
    }
    let mut roots = vec![out.join("Contents/Resources/nxkit")];
    if let Some(h) = home {
        let c = h.join("Library/Caches/nxkit");
        roots.push(c.join(title));
        roots.push(c.join(title.to_ascii_lowercase()));
    }
    if let Some(d) = env_dir {
        roots.push(d.to_path_buf());
    }
    let mut seen = std::collections::HashSet::new();
    roots
        .iter()
        .flat_map(|r| lists_in(r))
        .filter(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nxb-pipelist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("tmp");
        d
    }

    fn put(p: &Path) {
        std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
        std::fs::write(p, "nxkit-pipelines-seen 2\n").expect("write");
    }

    #[test]
    fn the_list_of_the_old_app_is_found_at_the_output_path() {
        let d = tmp("old-app");
        let out = d.join("Game.app");
        let run = out.join("Contents/Resources/nxkit/pipecache");
        put(&run.join("seen-3.list"));
        put(&run.join("id-0123/pipelines.seen"));
        put(&run.join("id-0123/other.txt"));
        put(&run.join("unrelated.list"));
        let got = find(None, &out, "0100ABCD", None, None);
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|p| p.ends_with("seen-3.list")));
        assert!(got.iter().any(|p| p.ends_with("id-0123/pipelines.seen")));
    }

    #[test]
    fn the_cache_folder_of_the_title_in_the_library_is_searched_too() {
        let d = tmp("library");
        let home = d.join("home");
        put(&home.join("Library/Caches/nxkit/0100ABCD/pipecache/seen-3.list"));
        let got = find(None, &d.join("none.app"), "0100ABCD", Some(&home), None);
        assert_eq!(got.len(), 1);
        let env = d.join("env");
        put(&env.join("pipecache/seen-3.list"));
        let both = find(
            None,
            &d.join("none.app"),
            "0100ABCD",
            Some(&home),
            Some(&env),
        );
        assert_eq!(both.len(), 2);
    }

    #[test]
    fn a_named_list_replaces_the_search() {
        let d = tmp("named");
        let out = d.join("Game.app");
        put(&out.join("Contents/Resources/nxkit/pipecache/seen-3.list"));
        let file = d.join("mine.list");
        assert_eq!(find(Some(&file), &out, "T", None, None), vec![file]);
    }

    #[test]
    fn no_app_and_no_cache_find_nothing() {
        let d = tmp("none");
        assert!(find(None, &d.join("x.app"), "T", Some(&d), None).is_empty());
    }
}
