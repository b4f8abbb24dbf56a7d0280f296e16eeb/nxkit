//! The save of an app of the old engine (nxnative) goes into the new app (issue 43).

use super::nxnative::{carry, current_save, device, is_app};
use std::fs;
use std::path::{Path, PathBuf};

const ID: u64 = 0x0100_F2C0_115B_6000;
const IDS: &str = "0100F2C0115B6000";

/// A home folder, a folder that holds the nxnative app, and the run folder of the new app.
struct Case {
    base: PathBuf,
    home: PathBuf,
    app: PathBuf,
    run: PathBuf,
}

impl Case {
    fn new(name: &str) -> Case {
        let base = std::env::temp_dir().join(format!("nxb-nxnative-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let app = base.join("games/Game.app");
        file(&app.join("Contents/MacOS/nxnative"), b"");
        fs::create_dir_all(app.join("Contents/Resources/dm")).unwrap();
        let run = base.join("new.app.partial/Contents/Resources/nxkit");
        fs::create_dir_all(&run).unwrap();
        Case {
            home: base.join("home"),
            app,
            run,
            base,
        }
    }

    /// The save folder of nxnative in the home folder.
    fn home_save(&self) -> PathBuf {
        self.home
            .join("Library/Application Support/nxnative")
            .join(IDS)
            .join("save")
    }

    /// The save folder of nxnative in the game library that holds the app.
    fn library_save(&self) -> PathBuf {
        fs::write(self.base.join("games/.nx2apple-library"), b"").unwrap();
        self.base.join("games/save").join(IDS)
    }

    fn carry(&self) -> Option<String> {
        carry(&self.home, &self.app, ID, &self.run).unwrap()
    }

    fn new_save(&self) -> PathBuf {
        self.run.join("save").join(IDS)
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn file(p: &Path, data: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

#[test]
fn an_app_of_the_old_engine_is_known() {
    let c = Case::new("is-app");
    assert!(is_app(&c.app));
    fs::remove_dir_all(c.app.join("Contents/Resources/dm")).unwrap();
    assert!(!is_app(&c.app));
}

#[test]
fn the_committed_save_and_the_device_save_are_copied() {
    let c = Case::new("home");
    let save = c.home_save();
    file(&save.join("progress.sav"), b"committed");
    file(&save.join("slot/1.bin"), b"one");
    file(&device(&save).join("device.sav"), b"device");
    // the work folder holds what the game wrote after its last commit: nxnative drops it, and so does the copy
    file(
        &save.with_extension("work").join("progress.sav"),
        b"not committed",
    );
    let said = c.carry().expect("a save to copy");
    assert!(said.contains(&*save.to_string_lossy()), "{said}");
    let new = c.new_save();
    assert_eq!(fs::read(new.join("progress.sav")).unwrap(), b"committed");
    assert_eq!(fs::read(new.join("slot/1.bin")).unwrap(), b"one");
    assert_eq!(
        fs::read(device(&new).join("device.sav")).unwrap(),
        b"device"
    );
    assert!(!c.run.join("save").join(format!("{IDS}.work")).exists());
    // a copy: the old save stays for an older build
    assert_eq!(fs::read(save.join("progress.sav")).unwrap(), b"committed");
}

#[test]
fn no_save_copies_nothing() {
    let c = Case::new("none");
    assert_eq!(c.carry(), None);
    // only what macOS writes into a folder is no save
    file(&c.home_save().join(".DS_Store"), b"");
    file(&c.home_save().join("._x"), b"");
    assert_eq!(current_save(&c.home, &c.app, ID), None);
    assert_eq!(c.carry(), None);
    assert!(!c.run.join("save").exists());
}

#[test]
fn the_library_save_is_current_before_the_home_save() {
    let c = Case::new("library");
    file(&c.home_save().join("progress.sav"), b"home");
    let lib = c.library_save();
    file(&lib.join("progress.sav"), b"library");
    assert_eq!(current_save(&c.home, &c.app, ID), Some(lib));
    c.carry().unwrap();
    assert_eq!(
        fs::read(c.new_save().join("progress.sav")).unwrap(),
        b"library"
    );
}

#[test]
fn an_empty_library_save_leaves_the_home_save() {
    // a library on a volume that is not APFS: nxnative saved in the home folder
    let c = Case::new("library-empty");
    fs::create_dir_all(c.library_save()).unwrap();
    file(&c.home_save().join("progress.sav"), b"home");
    assert_eq!(current_save(&c.home, &c.app, ID), Some(c.home_save()));
}

#[test]
fn the_recorded_save_location_wins() {
    let c = Case::new("recorded");
    let lib = c.library_save();
    file(&lib.join("progress.sav"), b"library");
    let chosen = c.base.join("chosen");
    file(&chosen.join("progress.sav"), b"last played");
    let rec = c.home_save().with_file_name("save-location");
    file(&rec, format!("{}\n", chosen.display()).as_bytes());
    assert_eq!(current_save(&c.home, &c.app, ID), Some(chosen.clone()));
    // a recorded folder that is gone: the library's save again
    fs::remove_dir_all(&chosen).unwrap();
    assert_eq!(current_save(&c.home, &c.app, ID), Some(lib));
}

/// A guest name that collides on APFS is an attribute of the entry, and it comes along.
#[cfg(target_os = "macos")]
#[test]
fn the_guest_names_come_along() {
    use std::process::Command;
    let c = Case::new("names");
    let dir = c.home_save().join(".dm-case-1");
    file(&dir.join("a.bin"), b"a");
    let ok = Command::new("/usr/bin/xattr")
        .args(["-w", "org.d2rmobile.guest-name", "Slot"])
        .arg(&dir)
        .status()
        .unwrap();
    assert!(ok.success());
    c.carry().unwrap();
    let out = Command::new("/usr/bin/xattr")
        .args(["-p", "org.d2rmobile.guest-name"])
        .arg(c.new_save().join(".dm-case-1"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Slot");
}
