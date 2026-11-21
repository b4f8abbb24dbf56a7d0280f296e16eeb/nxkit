//! The cloud's file: fetched with a time limit and stored as a whole. Plain file copies, which the Mac's iCloud Drive
//! takes (it downloads a file that a read asks for), as the weak defaults of `save_sync.c`.

use crate::files::sibling;
use crate::red;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

/// What a fetch found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fetched {
    /// The cloud's file is at the local path.
    Got,
    /// There is none.
    None,
    /// It could not be read now, or it did not come in time.
    Failed,
}

/// Copies the cloud's file to `local`.
fn fetch(cloud: &Path, local: &Path) -> Fetched {
    match std::fs::copy(cloud, local) {
        Ok(_) => Fetched::Got,
        Err(e) if e.kind() == io::ErrorKind::NotFound && !local.exists() => Fetched::None,
        Err(_) => {
            let _ = std::fs::remove_file(local);
            Fetched::Failed
        }
    }
}

/// Puts `local` at the cloud path as a whole: written beside it under a `.nosync` name, which iCloud Drive does not
/// upload, and renamed over it, so iCloud sees one new file.
pub(crate) fn store(local: &Path, cloud: &Path) -> io::Result<()> {
    let part = sibling(cloud, &format!(".{}.nosync", std::process::id()));
    let r = (|| {
        std::fs::copy(local, &part)?;
        std::fs::File::open(&part)?.sync_all()?;
        std::fs::rename(&part, cloud)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r
}

/// The fetch, given at most `wait`: a download that never finishes must not hold the game at its first save mount.
/// Each fetch writes a file of its own (`<local>.<pid>-<n>.part`), renamed to `local` only while the caller still
/// waits. A fetch that comes too late removes its own file and nothing else.
pub(crate) fn fetch_timed(cloud: &Path, local: &Path, wait: Duration) -> Fetched {
    // Process-wide (W26): a counter that names each fetch's file apart, also for fetches of different sessions.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let part = sibling(
        local,
        &format!(
            ".{}-{}.part",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ),
    );
    // (the answer, whether the caller stopped waiting)
    let job = Arc::new((Mutex::new((None::<Fetched>, false)), Condvar::new()));
    let (from, to, theirs): (PathBuf, PathBuf, _) =
        (cloud.to_path_buf(), part.clone(), Arc::clone(&job));
    let spawned = std::thread::Builder::new()
        .name("nxkit-save-fetch".into())
        .spawn(move || {
            let r = fetch(&from, &to);
            let (m, cv) = &*theirs;
            let mut g = m.lock().unwrap_or_else(PoisonError::into_inner);
            g.0 = Some(r);
            if g.1 {
                let _ = std::fs::remove_file(&to);
            }
            cv.notify_all();
        });
    if let Err(e) = spawned {
        red(&format!("no thread to fetch the cloud's save ({e})"));
        return Fetched::Failed;
    }
    let (m, cv) = &*job;
    let g = m.lock().unwrap_or_else(PoisonError::into_inner);
    let (mut g, _) = cv
        .wait_timeout_while(g, wait, |s| s.0.is_none())
        .unwrap_or_else(PoisonError::into_inner);
    let Some(r) = g.0 else {
        g.1 = true;
        crate::log(&format!(
            "the cloud's save did not come within {} s",
            wait.as_secs()
        ));
        return Fetched::Failed;
    };
    drop(g);
    if r == Fetched::Got {
        if let Err(e) = std::fs::rename(&part, local) {
            red(&format!(
                "cannot put the cloud's save at {} ({e})",
                local.display()
            ));
            let _ = std::fs::remove_file(&part);
            return Fetched::Failed;
        }
    } else {
        let _ = std::fs::remove_file(&part);
    }
    r
}
