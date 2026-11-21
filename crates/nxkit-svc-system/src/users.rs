//! The local users and their registry (R68), as `dm/host/account_users.h` and `hle_account_users.c` of the old engine.
//!
//! The host has up to [`USERS`] local users. User 1 has the uid of the one user before (`{UID0, UID1}`), user `n` the
//! uid `{UID0 + n - 1, UID1}`. Each user has a save of its own, which `nxkit-svc-fs` puts next to the save of user 1.
//!
//! The registry is the file `<save>.users` next to the save of user 1, in the format of the old engine:
//! `nxnative-users-1 <mask>\n`, the mask in hex with one bit per user. A missing file means user 1 alone. A new user
//! is written to a file beside it, synced and renamed over it, before a game sees the user. A file that cannot be read
//! is left as it is: user 1 stays, and no user is added.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

/// The number of local users.
pub(crate) const USERS: usize = 8;

/// The first word of the uid of user 1. User `n` adds `n - 1`.
const UID0: u64 = 0x7862_6c00_0000_0001;

/// The second word of every local uid.
const UID1: u64 = 0x5357_4e41_5449_5645;

/// The first word of the registry file.
const MAGIC: &str = "nxnative-users-1";

/// The uid of the user in `slot` (0 is user 1).
pub(crate) fn uid(slot: usize) -> [u64; 2] {
    [UID0 + slot as u64, UID1]
}

/// The slot of a local uid, registered or not.
pub(crate) fn slot_of(uid: [u64; 2]) -> Option<usize> {
    let i = uid[0].wrapping_sub(UID0);
    (uid[1] == UID1 && i < USERS as u64).then_some(i as usize)
}

/// The mask of a registry file, or `None` when the text is not one. User 1 must be in it.
fn parse(text: &str) -> Option<u8> {
    let mut words = text.split_whitespace();
    let mask = match (words.next(), words.next(), words.next()) {
        (Some(MAGIC), Some(m), None) => u8::from_str_radix(m, 16).ok()?,
        _ => return None,
    };
    (mask & 1 != 0).then_some(mask)
}

/// The text of a registry file.
fn format(mask: u8) -> String {
    format!("{MAGIC} {mask:02x}\n")
}

/// The nickname of the user in `slot`: `first` for user 1, `Player <n>` for the others.
pub(crate) fn nickname(slot: usize, first: &str) -> String {
    if slot == 0 {
        first.to_owned()
    } else {
        format!("Player {}", slot + 1)
    }
}

/// What the users are doing in this process.
#[derive(Debug)]
struct State {
    /// One bit per registered user.
    mask: u8,
    /// Whether a new user may be written. False without a file path or after a file that could not be read.
    writable: bool,
    /// The open sessions of each user.
    open: [u32; USERS],
    /// The user that was opened last, the launch user at the start.
    last: usize,
}

/// The registry and the open sessions.
#[derive(Debug)]
pub(crate) struct Users {
    path: Option<PathBuf>,
    launch: usize,
    state: Mutex<State>,
}

impl Users {
    /// Reads the registry at `path`. `launch` is the slot of the launch user, which is registered when it is new.
    pub(crate) fn load(path: Option<&Path>, launch: usize) -> Users {
        let read = path.map(|p| std::fs::read_to_string(p).map(|t| parse(&t)));
        let (mask, writable) = match read {
            None => (1, false),
            Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => (1, true),
            Some(Ok(Some(mask))) => (mask, true),
            Some(_) => {
                eprintln!(
                    "nxkit-svc-system: warning: the user registry {} cannot be read, so it stays as it is: user 1 \
                     plays, and no user is added",
                    path.map_or_else(String::new, |p| p.display().to_string())
                );
                (1, false)
            }
        };
        let users = Users {
            path: path.map(Path::to_path_buf),
            launch: 0,
            state: Mutex::new(State {
                mask,
                writable,
                open: [0; USERS],
                last: 0,
            }),
        };
        let launch = if launch == 0 || users.create(launch) {
            launch
        } else {
            eprintln!(
                "nxkit-svc-system: warning: user {} cannot be added, so user 1 plays",
                launch + 1
            );
            0
        };
        users.lock().last = launch;
        Users { launch, ..users }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The slot of the launch user (the preselected user).
    pub(crate) fn launch(&self) -> usize {
        self.launch
    }

    /// One bit per registered user.
    pub(crate) fn mask(&self) -> u8 {
        self.lock().mask
    }

    /// The registered users, user 1 first.
    pub(crate) fn registered(&self) -> Vec<usize> {
        let mask = self.mask();
        (0..USERS).filter(|i| mask & (1 << i) != 0).collect()
    }

    /// The users with an open session.
    pub(crate) fn opened(&self) -> Vec<usize> {
        let s = self.lock();
        (0..USERS).filter(|&i| s.open[i] > 0).collect()
    }

    /// The slot of a registered uid.
    pub(crate) fn user(&self, uid: [u64; 2]) -> Option<usize> {
        slot_of(uid).filter(|i| self.mask() & (1 << i) != 0)
    }

    /// Registers `slot` in the file before anything sees it. True when it is registered now or was before.
    pub(crate) fn create(&self, slot: usize) -> bool {
        if slot >= USERS {
            return false;
        }
        let mut s = self.lock();
        if s.mask & (1 << slot) != 0 {
            return true;
        }
        let (Some(path), true) = (&self.path, s.writable) else {
            return false;
        };
        let mask = s.mask | (1 << slot);
        match write_registry(path, mask) {
            Ok(()) => {
                s.mask = mask;
                eprintln!(
                    "nxkit-svc-system: user {} added ({})",
                    slot + 1,
                    path.display()
                );
                true
            }
            Err(e) => {
                eprintln!(
                    "nxkit-svc-system: warning: user {} cannot be written to {}: {e}",
                    slot + 1,
                    path.display()
                );
                false
            }
        }
    }

    /// Opens a session of a registered user. Gives its slot.
    pub(crate) fn open(&self, uid: [u64; 2]) -> Option<usize> {
        let slot = self.user(uid)?;
        let mut s = self.lock();
        s.open[slot] += 1;
        s.last = slot;
        Some(slot)
    }

    /// Closes one session of the user of `uid`.
    pub(crate) fn close(&self, uid: [u64; 2]) {
        if let Some(slot) = slot_of(uid) {
            let mut s = self.lock();
            s.open[slot] = s.open[slot].saturating_sub(1);
        }
    }

    /// The user that was opened last.
    pub(crate) fn last(&self) -> usize {
        self.lock().last
    }
}

fn write_registry(path: &Path, mask: u8) -> std::io::Result<()> {
    let tmp = {
        let mut s = path.as_os_str().to_os_string();
        s.push(format!(".{}", std::process::id()));
        PathBuf::from(s)
    };
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(format(mask).as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

// Process-wide (W26): the registry and the open sessions, which the account exports (no Rust context) and the fs
// lookup of the host share.
static USERS_STATE: OnceLock<Users> = OnceLock::new();

/// Loads the registry. The first call wins.
pub(crate) fn init(path: Option<&Path>, launch: usize) {
    let _ = USERS_STATE.set(Users::load(path, launch));
}

/// The users. Without `init`, user 1 alone, with no file.
pub(crate) fn users() -> &'static Users {
    USERS_STATE.get_or_init(|| Users::load(None, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nxkit-users-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("save.users")
    }

    #[test]
    fn a_uid_names_a_slot_and_back() {
        assert_eq!(uid(0), [0x7862_6c00_0000_0001, 0x5357_4e41_5449_5645]);
        assert_eq!(slot_of(uid(7)), Some(7));
        assert_eq!(slot_of([UID0 + 8, UID1]), None);
        assert_eq!(slot_of([UID0 - 1, UID1]), None);
        assert_eq!(slot_of([UID0, 0]), None);
    }

    #[test]
    fn the_registry_has_the_format_of_the_old_engine() {
        assert_eq!(format(0x05), "nxnative-users-1 05\n");
        assert_eq!(parse("nxnative-users-1 05\n"), Some(5));
        assert_eq!(parse("nxnative-users-1 ff"), Some(0xff));
        for bad in [
            "nxnative-users-1 04",
            "nxnative-users-1 100",
            "x 01",
            "nxnative-users-1 01 x",
            "",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_new_launch_user_is_written_before_it_plays() {
        let path = scratch("launch");
        let users = Users::load(Some(&path), 2);
        assert_eq!(users.launch(), 2);
        assert_eq!(users.registered(), [0, 2]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "nxnative-users-1 05\n"
        );
        let again = Users::load(Some(&path), 0);
        assert_eq!((again.registered(), again.last()), (vec![0, 2], 0));
        assert_eq!(again.user(uid(2)), Some(2));
        assert_eq!(again.user(uid(1)), None);
    }

    #[test]
    fn a_broken_registry_keeps_user_one_and_adds_nobody() {
        let path = scratch("broken");
        std::fs::write(&path, "garbage").unwrap();
        let users = Users::load(Some(&path), 3);
        assert_eq!((users.launch(), users.registered()), (0, vec![0]));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "garbage");
        assert!(!Users::load(None, 0).create(1));
    }

    #[test]
    fn sessions_open_and_close_per_user() {
        let users = Users::load(Some(&scratch("open")), 1);
        assert_eq!(users.open(uid(5)), None);
        assert_eq!(users.open(uid(0)), Some(0));
        assert_eq!(users.open(uid(1)), Some(1));
        assert_eq!((users.opened(), users.last()), (vec![0, 1], 1));
        users.close(uid(1));
        assert_eq!(users.opened(), [0]);
        assert_eq!(nickname(0, "Ann"), "Ann");
        assert_eq!(nickname(2, "Ann"), "Player 3");
    }
}
