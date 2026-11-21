//! Cloud saves of the nxkit host (R69): the Mac app and the iPhone app of one game play on from the same save through a
//! cloud folder such as iCloud Drive. A port of `dm/host/save_sync.c` of the old engine, byte-compatible with it: the
//! same packed file, folder layout, sync state, backups, decisions and report line, so an app of either engine on any
//! device shares the save.
//!
//! The folder holds one file per game, `<folder>/<title ID>/save.dmsync`: the committed save (`<save>` and
//! `<save>_device`) packed into one file ([`pack()`], [`read()`]). iCloud replaces a file as a whole but syncs a folder
//! file by file, and a save that is half one device's and half the other's is worse than either.
//!
//! When: the cloud's save comes in at the game's first save mount, before the work folder is cloned
//! ([`Session::mount`]). The committed save goes out a few seconds after a commit, on a thread of its own, so the game
//! never waits ([`Session::committed`]), and at the end of the game ([`Session::stop`], [`after_exit`]).
//!
//! Which way: `<save>.sync` keeps the hash of the save last known to be the same here and in the cloud. Only the
//! cloud's changed: it comes in. Only this one changed: it goes out. Both changed: the start decides ([`decide()`]),
//! by the player's [`Choice`] when a front end asks ([`report()`]), else the newer one wins. Every save this replaces is
//! kept first in `<save>-backups/<date> <side>`, and the newest 10 of each side stay.
//!
//! The crate works on folders. It knows nothing of the fs service: the host passes the clone of a committed save that
//! no commit can swap in [`Config::clone`], and calls the session from the hooks of the fs service.

mod decide;
mod fetch;
mod files;
mod format;
mod pack;
mod pull;
mod report;
mod send;
mod session;
mod state;
mod sys;
mod unpack;

#[cfg(all(test, target_vendor = "apple"))]
mod compat_tests;
#[cfg(all(test, target_vendor = "apple"))]
mod session_tests;

pub use decide::{Choice, Way, decide};
pub use format::{HASH_LEN, is_hash};
pub use pack::{pack, scan};
pub use report::report;
pub use session::Session;
pub use unpack::read;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// A game often commits several times in a row: the sender waits until it has been quiet this long.
const QUIET: Duration = Duration::from_secs(3);
/// The longest the game waits for the cloud's copy. Then it plays on with this device's save, which is not sent until
/// the two have been compared.
const FETCH: Duration = Duration::from_secs(10);

/// What a save holds, from the disk or from a packed file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// 32 hex digits over the paths, the guest names and the contents, not the dates. Empty for no save.
    pub hash: String,
    /// The newest file's modification time in seconds since 1970: which of two changed saves is newer.
    pub saved: i64,
    /// The title ID of a packed save, in capitals.
    pub title: String,
    /// The model of the device that packed it, for example `Mac16,1` or `iPhone17,1`.
    pub device: String,
    /// The regular files. 0: no save.
    pub files: u32,
    /// The size of their contents.
    pub bytes: i64,
}

/// Clones the committed save folder (first path) to a new folder (second path) while no commit can swap it.
pub type CloneFn = Arc<dyn Fn(&Path, &Path) -> io::Result<()> + Send + Sync>;

/// The clone of a folder with no game running: `clonefile`, with no lock.
pub fn plain_clone() -> CloneFn {
    Arc::new(sys::clone_dir)
}

/// What a session needs.
#[derive(Clone)]
pub struct Config {
    /// The committed save of user 1. Its device save is `<save>_device`.
    pub save: PathBuf,
    /// The title ID, which names the cloud's file.
    pub title: String,
    /// The sync setting (`NXKIT_SAVE_SYNC`): a folder, `icloud`, or `0` and `None` for off.
    pub folder: Option<String>,
    /// The home folder, which holds iCloud Drive.
    pub home: Option<PathBuf>,
    /// The player's choice before the start (`NXKIT_SAVE_SYNC_CHOICE`).
    pub choice: Choice,
    /// The clone of a committed save that no commit swaps meanwhile.
    pub clone: CloneFn,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("save", &self.save)
            .field("title", &self.title)
            .field("folder", &self.folder)
            .field("choice", &self.choice)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// The cloud's file, `None` when sync is off.
    pub(crate) fn cloud_file(&self) -> Option<PathBuf> {
        state::cloud_path(self.folder.as_deref(), &self.title, self.home.as_deref())
    }
}

/// The path without trailing slashes, as the old engine strips them.
pub(crate) fn trimmed(p: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let s = p.as_os_str().as_bytes();
    let mut n = s.len();
    while n > 1 && s[n - 1] == b'/' {
        n -= 1;
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&s[..n]))
}

/// Sends what the host committed and did not send, after the game ended (a crash included). True: in step with the
/// cloud, or sync is off.
pub fn after_exit(cfg: &Config) -> bool {
    let save = trimmed(&cfg.save);
    if !matches!(pull::journal_read(&save), Ok(None)) {
        red(
            "a pull of the cloud's save did not finish: nothing is sent until the next start puts this device's save back",
        );
        return false;
    }
    match cfg.cloud_file() {
        None => true,
        Some(cloud) => send::send(
            &save,
            &cfg.title,
            &cloud,
            &cfg.clone,
            &std::sync::atomic::AtomicBool::new(false),
        ),
    }
}

/// A line of the log.
pub(crate) fn log(msg: &str) {
    eprintln!("nxkit-save-sync: {msg}");
}

/// A line of the log about something that went wrong.
pub(crate) fn red(msg: &str) {
    eprintln!("nxkit-save-sync: warning: {msg}");
}
