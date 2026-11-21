//! The session of a running game: the first save mount, the sender after commits, the flush and the stop. As
//! `save_sync_mount`, `sender`, `save_sync_committed`, `save_sync_flush` and `dm_save_sync_stop` of `save_sync.c`.

use crate::decide::{Choice, Way, apply, decide};
use crate::fetch::{self, Fetched};
use crate::files::{self, sibling};
use crate::pull::{self, PullError};
use crate::state::{self, read_base, when, write_base};
use crate::{Config, Meta, QUIET, log, pack, red, send, unpack};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What the sender knows.
#[derive(Default)]
struct Sender {
    commits: u64,
    sent: u64,
    want: bool,
    busy: bool,
    failed: bool,
    stop: bool,
}

/// The cloud saves of one running game.
pub struct Session {
    cfg: Config,
    /// The answer of the first mount: `None` before it ran.
    mounted: Mutex<Option<bool>>,
    /// The cloud's file, once the first mount found sync on.
    cloud: Mutex<Option<PathBuf>>,
    sender: Mutex<Sender>,
    cv: Condvar,
    thread: Mutex<Option<JoinHandle<()>>>,
    told: AtomicBool,
}

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Session {
    /// A session for `cfg`. Nothing happens before the first mount.
    pub fn new(mut cfg: Config) -> Arc<Session> {
        cfg.save = crate::trimmed(&cfg.save);
        Arc::new(Session {
            cfg,
            mounted: Mutex::new(None),
            cloud: Mutex::new(None),
            sender: Mutex::new(Sender::default()),
            cv: Condvar::new(),
            thread: Mutex::new(None),
            told: AtomicBool::new(false),
        })
    }

    /// The game's first save mount, before the work folder is cloned: puts back this device's save from a pull that did
    /// not finish, then brings the cloud's save in when it is the one to keep, and starts the sender. An error: the save
    /// must not be mounted, as this device's save could not be put back whole. A second mount gets the first's answer.
    pub fn mount(self: &Arc<Self>) -> Result<(), String> {
        let mut m = guard(&self.mounted);
        let ok = *m.get_or_insert_with(|| match self.mount_once() {
            Ok(()) => true,
            Err(e) => {
                red(&e);
                false
            }
        });
        if ok {
            Ok(())
        } else {
            Err(format!(
                "the save {} is not whole after a pull of the cloud's save that could not be undone (the lines of \
                 nxkit-save-sync above say where this device's save is kept; the next start tries again)",
                self.cfg.save.display()
            ))
        }
    }

    fn mount_once(self: &Arc<Self>) -> Result<(), String> {
        let save = &self.cfg.save;
        match pull::journal_read(save) {
            Err(e) => {
                return Err(format!(
                    "{}, the journal of a pull of the cloud's save, cannot be read ({e}): the save is not mounted; \
                     this device's save is in place or in {}",
                    sibling(save, ".pull").display(),
                    sibling(save, "-backups").display()
                ));
            }
            Ok(Some(j)) => {
                log("a pull of the cloud's save did not finish: putting this device's save back");
                pull::recover(save, &j).map_err(|e| {
                    format!(
                        "this device's save could not be put back ({e}): it is not mounted; what is not in {} is kept \
                         in {}, and the next start tries again",
                        save.display(),
                        j.kept.display()
                    )
                })?;
                log("this device's save is back as it was before that pull");
            }
            Ok(None) => {}
        }
        let Some(cloud) = self.cfg.cloud_file() else {
            return Ok(());
        };
        log(&format!("on: {}", cloud.display()));
        let tmp = sibling(save, ".sync-tmp");
        if let Err(e) = files::rm_tree(&tmp).and_then(|()| std::fs::create_dir_all(&tmp)) {
            red(&format!("cannot make {} ({e}): off", tmp.display()));
            return Ok(());
        }
        let r = self.compare(&cloud, &tmp.join("in.dmsync"));
        let _ = files::rm_tree(&tmp);
        let failed = r?;
        *guard(&self.cloud) = Some(cloud);
        {
            let mut s = guard(&self.sender);
            s.failed = failed;
        }
        let me = Arc::clone(self);
        match std::thread::Builder::new()
            .name("nxkit-save-send".into())
            .spawn(move || me.run_sender())
        {
            Ok(t) => *guard(&self.thread) = Some(t),
            Err(_) => red("cannot start the sender: saves are not sent this session"),
        }
        Ok(())
    }

    /// Compares this device's save with the cloud's and brings the cloud's in when it wins. Gives whether the two are
    /// still apart (nothing is sent before the next start compares them).
    fn compare(&self, cloud: &Path, theirs: &Path) -> Result<bool, String> {
        let save = &self.cfg.save;
        let title = &self.cfg.title;
        let mine = match pack::scan(save) {
            Ok(m) => m,
            Err(e) => {
                red(&format!(
                    "this device's save {} cannot be read ({e}): off",
                    save.display()
                ));
                return Ok(true);
            }
        };
        let base = read_base(save);
        let choice = &self.cfg.choice;
        let mut got = if *choice == Choice::Offline {
            Fetched::Failed
        } else {
            fetch::fetch_timed(cloud, theirs, crate::FETCH)
        };
        let mut other = Meta::default();
        if got == Fetched::Got {
            match unpack::read(theirs, None) {
                Err(_) => {
                    red(&format!(
                        "the cloud's save {} is damaged: playing on with this device's, which replaces it (the damaged \
                         file is kept)",
                        cloud.display()
                    ));
                    got = Fetched::None;
                }
                Ok(o) if !o.title.eq_ignore_ascii_case(title) => {
                    red(&format!(
                        "the cloud's save {} is {}'s, not {title}'s: not used",
                        cloud.display(),
                        o.title
                    ));
                    got = Fetched::Failed;
                }
                Ok(o) => other = o,
            }
        }
        let mut failed = false;
        if got == Fetched::Failed {
            log(
                "playing on with this device's save; it is not sent until it has been compared with the cloud's",
            );
            failed = true;
        }
        let remote = if got == Fetched::Got {
            other.hash.clone()
        } else {
            String::new()
        };
        let (mut way, both) = if got == Fetched::Failed {
            (Way::Keep, false)
        } else {
            decide(
                &mine.hash,
                &base,
                &remote,
                mine.saved,
                other.saved,
                choice.asks(),
            )
        };
        let now = if got == Fetched::Got && other.files > 0 {
            other.hash.as_str()
        } else {
            ""
        };
        if matches!(choice, Choice::Cloud(_) | Choice::Local(_))
            && got != Fetched::Failed
            && way != Way::Keep
        {
            let (w, used) = apply(choice, way, now);
            if !used {
                red(&format!(
                    "the cloud's save changed after the player chose (now {}): the choice is not used",
                    if now.is_empty() { "none" } else { now }
                ));
            }
            way = w;
        } else if *choice == Choice::Offline {
            log("the player chose to play with this device's save without the cloud's");
        }
        match way {
            Way::Pull => match pull::pull(save, theirs) {
                Ok((pulled, kept)) => {
                    write_base(save, &pulled.hash);
                    let why = match choice {
                        Choice::Cloud(_) => ", the player's choice",
                        _ if both => ", the newer of two that both changed",
                        _ => "",
                    };
                    let kept = if kept.as_os_str().is_empty() {
                        String::new()
                    } else {
                        format!("; this device's is kept in {}", kept.display())
                    };
                    log(&format!(
                        "brought in the cloud's save (from {}, saved {}{why}){kept}",
                        pulled.device,
                        when(pulled.saved)
                    ));
                }
                Err(PullError::Unsafe(e, kept)) => {
                    return Err(format!(
                        "bringing the cloud's save in failed ({e}), and this device's could not all be put back: it \
                         is not mounted; what is not in {} is kept in {}, and the next start tries again",
                        save.display(),
                        kept.display()
                    ));
                }
                Err(PullError::Unchanged(e)) => {
                    red(&format!(
                        "bringing the cloud's save in failed ({e}): playing on with this device's"
                    ));
                    failed = true;
                }
            },
            Way::Push => {
                // Over a cloud save that changed since the last sync: the start chose against it, so it is kept first
                // and counts as the one last synced.
                let mut kept = None;
                if got == Fetched::Got && other.files > 0 && other.hash != base {
                    let k = state::backup_dir(save, "cloud", true)
                        .and_then(|k| unpack::read(theirs, Some(&k)).map(|_| k));
                    match k {
                        Ok(k) => {
                            write_base(save, &other.hash);
                            kept = Some(k);
                        }
                        Err(e) => {
                            red(&format!(
                                "cannot keep the cloud's save before replacing it ({e}): not sent"
                            ));
                            return Ok(true);
                        }
                    }
                }
                let why = if got != Fetched::Got || other.files == 0 {
                    "is not in the cloud yet"
                } else if matches!(choice, Choice::Local(_)) {
                    "is the player's choice"
                } else if both {
                    "is the newer of two that both changed"
                } else {
                    "changed since the last sync"
                };
                log(&format!(
                    "this device's save (saved {}) {why}: it goes to the cloud",
                    when(mine.saved)
                ));
                if let Some(k) = kept {
                    log(&format!(
                        "the cloud's save it replaces (from {}, saved {}) is kept in {}",
                        other.device,
                        when(other.saved),
                        k.display()
                    ));
                }
                guard(&self.sender).want = true;
            }
            Way::Ask => {
                log(&format!(
                    "both changed: the cloud's save (from {}, saved {}) and this device's (saved {}); playing on with \
                     this device's, and neither replaces the other until the player chooses at the next start",
                    other.device,
                    when(other.saved),
                    when(mine.saved)
                ));
                failed = true;
            }
            Way::Keep => {
                if got == Fetched::Got && mine.files > 0 {
                    write_base(save, &mine.hash);
                    log("in step with the cloud");
                } else if got == Fetched::None {
                    log("no save here or in the cloud yet");
                }
            }
        }
        Ok(failed)
    }

    fn run_sender(&self) {
        let mut s = guard(&self.sender);
        loop {
            while s.commits == s.sent && !s.want && !s.stop {
                s = self.cv.wait(s).unwrap_or_else(PoisonError::into_inner);
            }
            if s.stop && s.commits == s.sent && !s.want {
                break;
            }
            // Quiet for a few seconds since the last commit: a game often commits several times in a row.
            let mut seen = s.commits;
            while !s.want && !s.stop {
                let until = Instant::now() + QUIET;
                while !s.want && !s.stop && s.commits == seen {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    s = self
                        .cv
                        .wait_timeout(s, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                if s.commits == seen {
                    break;
                }
                seen = s.commits;
            }
            if s.stop && s.commits == s.sent && !s.want {
                break;
            }
            let upto = s.commits;
            s.want = false;
            s.busy = true;
            drop(s);
            let cloud = guard(&self.cloud).clone();
            let ok = cloud.is_some_and(|c| {
                send::send(
                    &self.cfg.save,
                    &self.cfg.title,
                    &c,
                    &self.cfg.clone,
                    &self.told,
                )
            });
            s = guard(&self.sender);
            s.busy = false;
            s.sent = upto;
            s.failed = !ok;
            self.cv.notify_all();
            if s.stop {
                break;
            }
        }
    }

    /// A commit went through. It only wakes the sender.
    pub fn committed(&self) {
        let mut s = guard(&self.sender);
        s.commits += 1;
        self.cv.notify_all();
    }

    /// Sends now what was committed and not sent, waiting `timeout` at most. True: in step with the cloud.
    pub fn flush(&self, timeout: Duration) -> bool {
        if guard(&self.thread).is_none() {
            return true;
        }
        let mut s = guard(&self.sender);
        if s.commits != s.sent || s.failed || s.busy {
            s.want = true;
            self.cv.notify_all();
        }
        let (s, _) = self
            .cv
            .wait_timeout_while(s, timeout, |s| s.want || s.busy || s.commits != s.sent)
            .unwrap_or_else(PoisonError::into_inner);
        !s.want && !s.busy && s.commits == s.sent && !s.failed
    }

    /// The end of the game: a flush, then the sender stops. A send that is running finishes first.
    pub fn stop(&self) {
        if guard(&self.thread).is_none() {
            return;
        }
        self.flush(Duration::from_millis(500));
        {
            let mut s = guard(&self.sender);
            s.stop = true;
            self.cv.notify_all();
        }
        let thread = guard(&self.thread).take();
        if let Some(t) = thread {
            let _ = t.join();
        }
    }
}
