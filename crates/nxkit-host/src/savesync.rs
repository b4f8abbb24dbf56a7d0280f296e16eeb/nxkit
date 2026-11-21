//! The cloud saves of the running game (R69): `nxkit-save-sync` wired to the fs service and to the exit, and the
//! command line entries for a launcher.
//!
//! The fs service calls the hooks of [`Hooks`]: the first mount of the save of user 1 or of the device save runs the
//! sync of the start, and a commit wakes the sender. The sender clones the committed save through
//! `nxkit_svc_fs::clone_committed`, so no commit swaps it meanwhile. At the exit the session sends what is left.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use nxkit_save_sync::{Choice, Config, Session};

use crate::hostenv::HostEnv;
use crate::saveplace;

/// The configuration of the sync for the save `save` of the game `title_id`. A choice that is not one is ignored with
/// a line in the log.
pub fn config(
    save: &Path,
    title_id: u64,
    env: &HostEnv,
    clone: nxkit_save_sync::CloneFn,
) -> Config {
    let (choice, why) = Choice::parse(env.save_sync_choice.as_deref().unwrap_or(""));
    if let Some(why) = why {
        eprintln!("nxkit-host: {}={why}", nxkit_abi::launch::SAVE_SYNC_CHOICE);
    }
    Config {
        save: save.to_path_buf(),
        title: saveplace::title_folder(title_id),
        folder: env.save_sync.clone(),
        home: env.home.clone(),
        choice,
        clone,
    }
}

/// The hooks that the fs service calls.
pub struct Hooks(Arc<Session>);

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("savesync::Hooks")
    }
}

impl nxkit_svc_fs::SaveHooks for Hooks {
    fn first_mount(&self, _save: &Path) {
        if let Err(e) = self.0.mount() {
            crate::stop::fatal(&e);
        }
    }

    fn committed(&self, _dir: &Path) {
        self.0.committed();
    }
}

// Process-wide (W26): the session of the running game, for the exit handler, which has no context.
static SESSION: OnceLock<Arc<Session>> = OnceLock::new();

/// Starts the session of the running game and gives the hooks for the fs service.
pub fn start(cfg: Config) -> Arc<Hooks> {
    let session = Session::new(cfg);
    let _ = SESSION.set(Arc::clone(&session));
    Arc::new(Hooks(session))
}

/// The end of the process: sends what was committed and not sent, briefly.
pub fn at_exit() {
    if let Some(s) = SESSION.get() {
        s.stop();
    }
}

/// The save and the title of the app at `root`, as the game will have them.
fn save_of(root: &Path, env: &HostEnv) -> Result<(PathBuf, u64), String> {
    let path = root.join("game.json");
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let game = crate::manifest::Game::parse(root, &src).map_err(|e| e.to_string())?;
    let id = u64::from_str_radix(game.title_id.trim_start_matches("0x"), 16)
        .map_err(|_| format!("\"title_id\" {} is not a hex number", game.title_id))?;
    let places = saveplace::places(
        env.home.as_deref(),
        env.save.as_deref(),
        root,
        id,
        &game.title_id,
    );
    Ok((places.save, id))
}

/// `--save-sync-report [<ms>]`: prints the report line of the save of the app at `root` (the line of
/// `save_sync_report`). `wait` is how long the cloud's file may take. 0 on success, 1 when the save cannot be read.
pub fn report_cli(root: &Path, env: &HostEnv, wait: Duration) -> i32 {
    match save_of(root, env) {
        Ok((save, id)) => {
            let title = saveplace::title_folder(id);
            match nxkit_save_sync::report(
                &save,
                &title,
                env.save_sync.as_deref(),
                env.home.as_deref(),
                wait,
            ) {
                Some(line) => {
                    println!("{line}");
                    0
                }
                None => {
                    eprintln!("nxkit-host: the save {} cannot be read", save.display());
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("nxkit-host: {e}");
            1
        }
    }
}

/// `--save-sync-send`: sends what the game committed and did not send, after it ended (a crash included). 0: in step
/// with the cloud or sync off, 1: not (the next start sends it).
pub fn send_cli(root: &Path, env: &HostEnv) -> i32 {
    match save_of(root, env) {
        Ok((save, id)) => i32::from(!nxkit_save_sync::after_exit(&config(
            &save,
            id,
            env,
            nxkit_save_sync::plain_clone(),
        ))),
        Err(e) => {
            eprintln!("nxkit-host: {e}");
            1
        }
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_entries_find_the_save_of_the_app() {
        let d = std::env::temp_dir().join(format!("nxkit-host-savesync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let root = d.join("Game.app/Contents/Resources/nxkit");
        std::fs::create_dir_all(&root).unwrap();
        let json = crate::manifest::sample("", crate::module::samples::NEAR);
        std::fs::write(root.join("game.json"), json).unwrap();
        let env = HostEnv {
            home: Some(d.join("home")),
            save_sync: Some(d.join("cloud").to_string_lossy().into_owned()),
            ..HostEnv::default()
        };
        let (save, id) = save_of(&root, &env).unwrap();
        assert_eq!(
            save,
            d.join("home/Library/Application Support/nxkit/01007EF00011E000/save")
        );
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("game.dat"), "v1").unwrap();
        assert_eq!(report_cli(&root, &env, Duration::from_secs(2)), 0);
        assert_eq!(send_cli(&root, &env), 0);
        assert!(d.join("cloud/01007EF00011E000/save.dmsync").is_file());
        assert_eq!(
            config(&save, id, &env, nxkit_save_sync::plain_clone()).title,
            "01007EF00011E000"
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
