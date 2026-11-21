//! Two devices that share a cloud folder, as `tests/t_save_sync.c` of the old engine runs them: each session mounts,
//! saves (a file written into the committed save and a commit) and ends. The expected values follow `save_sync.h`.

// The lint does not see `cfg(all(test, ..))` as a test module.
#![allow(clippy::unwrap_used, reason = "test helpers")]

use crate::files::sibling;
use crate::{Choice, Config, Session, after_exit, plain_clone, report, unpack};
use std::path::{Path, PathBuf};
use std::time::Duration;

const TITLE: &str = "0100abc";

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxkit-save-sync-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(save: &Path, cloud: Option<&Path>, choice: Choice) -> Config {
    Config {
        save: save.to_path_buf(),
        title: TITLE.into(),
        folder: cloud.map(|c| c.to_string_lossy().into_owned()),
        home: None,
        choice,
        clone: plain_clone(),
    }
}

/// One session: the first mount, an optional save of `game.dat`, the end.
fn session(save: &Path, cloud: Option<&Path>, value: Option<&str>, choice: Choice) {
    std::fs::create_dir_all(save).unwrap();
    let s = Session::new(config(save, cloud, choice));
    s.mount().unwrap();
    if let Some(v) = value {
        std::fs::write(save.join("game.dat"), v).unwrap();
        s.committed();
    }
    s.stop();
}

fn here(save: &Path) -> String {
    std::fs::read_to_string(save.join("game.dat")).unwrap_or_default()
}

/// The `game.dat` of the cloud's save and its hash.
fn cloud(root: &Path, d: &Path) -> (String, String) {
    let into = d.join(format!("look-{}", rand_name()));
    let m = unpack::read(&root.join("0100ABC/save.dmsync"), Some(&into)).unwrap();
    (
        std::fs::read_to_string(into.join("save/game.dat")).unwrap(),
        m.hash,
    )
}

fn rand_name() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn backups(save: &Path, side: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(sibling(save, "-backups")) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(&format!(" {side}")))
        .collect();
    v.sort();
    v
}

#[test]
fn two_devices_play_on_from_one_save() {
    let d = scratch("two");
    let (a, b, root) = (d.join("mac/save"), d.join("phone/save"), d.join("cloud"));
    session(&a, Some(&root), Some("v1"), Choice::None);
    assert_eq!(
        cloud(&root, &d).0,
        "v1",
        "the Mac's first session sends its save"
    );
    assert!(sibling(&a, ".sync").exists() && !sibling(&a, ".sync-tmp").exists());
    session(&b, Some(&root), None, Choice::None);
    assert_eq!(here(&b), "v1", "the phone's first session brings it in");
    assert!(
        backups(&b, "local").is_empty(),
        "with no save of its own, the phone keeps no backup"
    );
    session(&b, Some(&root), Some("v2"), Choice::None);
    assert_eq!(cloud(&root, &d).0, "v2");
    session(&a, Some(&root), None, Choice::None);
    assert_eq!(
        here(&a),
        "v2",
        "the Mac starts again: the phone's save comes in"
    );
    let kept = backups(&a, "local");
    assert_eq!(kept.len(), 1);
    assert_eq!(
        here(&kept[0].join("save")),
        "v1",
        "the Mac's old save is kept"
    );
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn both_changed_waits_for_the_players_choice() {
    let d = scratch("choice");
    let (a, b, root) = (d.join("mac/save"), d.join("phone/save"), d.join("cloud"));
    session(&a, Some(&root), Some("v1"), Choice::None);
    session(&b, Some(&root), None, Choice::None);
    session(&b, None, Some("v4"), Choice::None);
    session(&a, Some(&root), Some("v5"), Choice::None);
    let line = report(
        &b,
        TITLE,
        Some(&root.to_string_lossy()),
        None,
        Duration::ZERO,
    )
    .unwrap();
    assert!(
        line.contains(" conflict=1 ") && line.starts_with("sync=1 cloud=1 "),
        "{line}"
    );
    session(&b, Some(&root), None, Choice::Ask);
    assert_eq!(
        (here(&b), cloud(&root, &d).0),
        ("v4".into(), "v5".into()),
        "with no choice neither replaces the other"
    );
    assert!(backups(&b, "local").is_empty() && backups(&b, "cloud").is_empty());
    let (_, hash) = cloud(&root, &d);
    session(&b, Some(&root), None, Choice::Cloud(hash));
    assert_eq!(here(&b), "v5", "the player chooses the cloud's save");
    assert_eq!(
        here(&backups(&b, "local")[0].join("save")),
        "v4",
        "the phone's own is kept"
    );
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn a_save_left_unsent_goes_after_the_exit() {
    let d = scratch("exit");
    let (a, root) = (d.join("mac/save"), d.join("cloud"));
    session(&a, Some(&root), Some("v1"), Choice::None);
    std::fs::write(a.join("game.dat"), "v8").unwrap();
    let cfg = config(&a, Some(&root), Choice::None);
    assert!(after_exit(&cfg));
    assert_eq!(cloud(&root, &d).0, "v8");
    assert!(
        after_exit(&config(&a, None, Choice::None)),
        "sync off: nothing to do"
    );
    std::fs::remove_dir_all(d).unwrap();
}
