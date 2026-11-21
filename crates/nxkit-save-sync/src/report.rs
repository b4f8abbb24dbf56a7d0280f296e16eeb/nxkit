//! The report a front end asks the player with before the start, as `save_sync_report` of `save_sync.c`: one line of
//! `key=value` pairs.
//!
//! ```text
//! sync=<0|1> cloud=<1 read | 0 no save there or a damaged file | -1 not read in time> local=<hash|-> saved=<secs>
//! files=<n> size=<bytes> chash=<hash|-> csaved=<secs> cfiles=<n> csize=<bytes> cdevice=<model|-> sametitle=<0|1>
//! conflict=<0|1> pull=<0|1>
//! ```

use crate::decide::decide;
use crate::fetch::{self, Fetched};
use crate::files::{self, sibling};
use crate::{Meta, pack, pull, red, state, unpack};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// The fields of the line.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) on: bool,
    pub(crate) got: Option<Fetched>,
    pub(crate) mine: Meta,
    pub(crate) other: Meta,
    pub(crate) same_title: bool,
    pub(crate) conflict: bool,
    pub(crate) pending: bool,
}

/// The line of a report.
pub(crate) fn line(r: &Report) -> String {
    let got = r.got == Some(Fetched::Got);
    let cloud = match (r.on, r.got) {
        (false, _) => 0,
        (true, Some(Fetched::Got)) => 1,
        (true, Some(Fetched::None)) => 0,
        (true, _) => -1,
    };
    let dash = |s: &str| {
        if s.is_empty() {
            "-".to_owned()
        } else {
            s.to_owned()
        }
    };
    let dev: String = r
        .other
        .device
        .chars()
        .map(|c| if c == ' ' || c == '=' { '_' } else { c })
        .collect();
    format!(
        "sync={} cloud={cloud} local={} saved={} files={} size={} chash={} csaved={} cfiles={} csize={} cdevice={} \
         sametitle={} conflict={} pull={}",
        u8::from(r.on),
        dash(&r.mine.hash),
        r.mine.saved,
        r.mine.files,
        r.mine.bytes,
        if got { dash(&r.other.hash) } else { "-".into() },
        r.other.saved,
        r.other.files,
        r.other.bytes,
        dash(&dev),
        u8::from(r.same_title),
        u8::from(r.conflict),
        u8::from(r.pending)
    )
}

/// The two saves and the one last synced, as the first mount will compare them. `folder` is the sync setting, `wait`
/// how long the cloud's file may take (zero: as long as the mount waits). `None` when this device's save cannot be
/// read.
pub fn report(
    save: &Path,
    title: &str,
    folder: Option<&str>,
    home: Option<&Path>,
    wait: Duration,
) -> Option<String> {
    // Process-wide (W26): a counter that names each report's file apart, as a front end may ask again while an
    // earlier fetch still runs.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let save = &crate::trimmed(save);
    if title.is_empty() {
        return None;
    }
    let mut r = Report {
        mine: pack::scan(save).ok()?,
        same_title: true,
        ..Report::default()
    };
    let base = state::read_base(save);
    if let Some(cloud) = state::cloud_path(folder, title, home) {
        r.on = true;
        r.got = Some(Fetched::Failed);
        let tmp = sibling(save, ".sync-tmp");
        let theirs = tmp.join(format!(
            "report-{}-{}.dmsync",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if std::fs::create_dir_all(&tmp).is_ok() {
            let wait = if wait.is_zero() { crate::FETCH } else { wait };
            let mut got = fetch::fetch_timed(&cloud, &theirs, wait);
            if got == Fetched::Got {
                match unpack::read(&theirs, None) {
                    Ok(o) => r.other = o,
                    Err(_) => {
                        red(&format!("the cloud's save {} is damaged", cloud.display()));
                        got = Fetched::None;
                    }
                }
            }
            r.got = Some(got);
            let _ = files::rm_tree(&theirs);
            let _ = std::fs::remove_dir(&tmp);
        }
        if r.got == Some(Fetched::Got) && !r.other.title.eq_ignore_ascii_case(title) {
            r.same_title = false;
        }
    }
    r.pending = !matches!(pull::journal_read(save), Ok(None));
    if r.got != Some(Fetched::Failed) && r.same_title && !r.pending {
        let remote = if r.got == Some(Fetched::Got) {
            r.other.hash.as_str()
        } else {
            ""
        };
        r.conflict = decide(
            &r.mine.hash,
            &base,
            remote,
            r.mine.saved,
            r.other.saved,
            true,
        )
        .1;
    }
    Some(line(&r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_has_the_keys_of_save_sync_report() {
        let off = Report {
            same_title: true,
            ..Report::default()
        };
        assert_eq!(
            line(&off),
            "sync=0 cloud=0 local=- saved=0 files=0 size=0 chash=- csaved=0 cfiles=0 csize=0 cdevice=- sametitle=1 \
             conflict=0 pull=0"
        );
        let on = Report {
            on: true,
            got: Some(Fetched::Got),
            mine: Meta {
                hash: "a".repeat(32),
                saved: 5,
                files: 2,
                bytes: 9,
                ..Meta::default()
            },
            other: Meta {
                hash: "b".repeat(32),
                saved: 6,
                files: 1,
                bytes: 3,
                device: "Mac 1=2".into(),
                ..Meta::default()
            },
            same_title: true,
            conflict: true,
            pending: false,
        };
        let l = line(&on);
        assert!(l.starts_with(&format!(
            "sync=1 cloud=1 local={} saved=5 files=2 size=9 chash={} ",
            "a".repeat(32),
            "b".repeat(32)
        )));
        assert!(
            l.ends_with("csaved=6 cfiles=1 csize=3 cdevice=Mac_1_2 sametitle=1 conflict=1 pull=0")
        );
        let late = Report {
            on: true,
            got: Some(Fetched::Failed),
            ..Report::default()
        };
        assert!(line(&late).starts_with("sync=1 cloud=-1 "));
    }
}
