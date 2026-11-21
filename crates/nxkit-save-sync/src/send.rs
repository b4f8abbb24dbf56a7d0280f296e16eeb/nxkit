//! The send: what this device committed goes to the cloud, unless the cloud already has it. As `send_save` of
//! `save_sync.c`.
//!
//! A send replaces only what the start compared against: no save, the save last synced (`<save>.sync`), or a damaged
//! file (kept first). A cloud save that changed since this device's last sync is not replaced. The next start decides
//! between the two.

use crate::fetch::{self, Fetched};
use crate::files::{self, folder, sibling};
use crate::state::{self, read_base, when, write_base};
use crate::{CloneFn, log, pack, red, unpack};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Sends the committed save `save`. `told` keeps the note of a cloud save that changed to once a session. True: in
/// step with the cloud. False: not now (the next commit, a flush or the next start tries again).
pub(crate) fn send(
    save: &Path,
    title: &str,
    cloud: &Path,
    clone: &CloneFn,
    told: &AtomicBool,
) -> bool {
    let tmp = sibling(save, ".sync-tmp").join("out");
    let ok = send_in(save, title, cloud, clone, told, &tmp);
    let _ = files::rm_tree(&tmp);
    if let Some(parent) = tmp.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    ok
}

fn send_in(
    save: &Path,
    title: &str,
    cloud: &Path,
    clone: &CloneFn,
    told: &AtomicBool,
    tmp: &Path,
) -> bool {
    let (snap, packed, theirs) = (
        tmp.join("s"),
        tmp.join("save.dmsync"),
        tmp.join("cloud.dmsync"),
    );
    if let Err(e) = files::rm_tree(tmp).and_then(|()| std::fs::create_dir_all(tmp)) {
        red(&format!("cannot make {} ({e})", tmp.display()));
        return false;
    }
    // A clone of the committed folders, taken while no commit can swap them: the game may commit again meanwhile.
    for i in 0..2 {
        let from = folder(save, i);
        match files::has_entries(&from) {
            Err(e) => {
                red(&format!(
                    "{} cannot be read ({e}): not sent",
                    from.display()
                ));
                return false;
            }
            Ok(false) => {}
            Ok(true) => {
                if let Err(e) = clone(&from, &folder(&snap, i)) {
                    red(&format!("cloning {} failed ({e})", from.display()));
                    return false;
                }
            }
        }
    }
    let mine = match pack::pack(&snap, title, &packed) {
        Ok(m) => m,
        Err(e) => {
            red(&format!("packing {} failed ({e})", save.display()));
            return false;
        }
    };
    let base = read_base(save);
    if mine.files == 0 || mine.hash == base {
        return true;
    }
    let got = fetch::fetch_timed(cloud, &theirs, crate::FETCH);
    if got == Fetched::Failed {
        log(&format!(
            "the cloud's save {} cannot be read now; sending later",
            cloud.display()
        ));
        return false;
    }
    if got == Fetched::Got {
        match unpack::read(&theirs, None) {
            Err(_) => {
                // No save can be read from it, so no progress is lost: the file is kept, then replaced.
                let kept = state::backup_dir(save, "cloud", true)
                    .and_then(|k| std::fs::rename(&theirs, k.join("save.dmsync")).map(|()| k));
                match kept {
                    Ok(k) => red(&format!(
                        "the cloud's save {} is damaged; this device's replaces it, and the damaged file is kept in {}",
                        cloud.display(),
                        k.display()
                    )),
                    Err(e) => {
                        red(&format!(
                            "cannot keep the cloud's damaged save before replacing it ({e}); not sent"
                        ));
                        return false;
                    }
                }
            }
            Ok(other) if other.hash == mine.hash => {
                write_base(save, &mine.hash);
                return true;
            }
            Ok(other) if other.files > 0 && other.hash != base => {
                if !told.swap(true, Ordering::Relaxed) {
                    log(&format!(
                        "not sent: the cloud's save (from {}, saved {}) changed since this device's last sync, and so \
                         did this one; the next start decides between the two",
                        other.device,
                        when(other.saved)
                    ));
                }
                return false;
            }
            Ok(_) => {}
        }
    }
    let stored = cloud
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| fetch::store(&packed, cloud));
    if let Err(e) = stored {
        red(&format!("sending to {} failed ({e})", cloud.display()));
        return false;
    }
    write_base(save, &mine.hash);
    log(&format!(
        "sent: {} files, saved {}, to {}",
        mine.files,
        when(mine.saved),
        cloud.display()
    ));
    true
}
