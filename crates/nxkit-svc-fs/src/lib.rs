//! The `fs` service of nxkit: `fsp-srv` over IPC and the `nn::fs` functions at SDK level.
//!
//! The game reaches the file system in two ways. The `nn::fs` functions of the SDK are replaced by the functions of
//! [`exports()`] and [`big_stack_exports()`], as `dm/host/hle_fs.c` does in the engine (R14). The IPC interfaces of
//! [`services()`] answer the SDK code that the engine does not replace (R35).
//!
//! The romfs is a folder that the build merged from the base, update and DLC layers. Its files are mapped and read
//! without a copy. The saves have the layout of the engine on disk (R13).
//!
//! The host calls [`init`] once, before the guest runs, and hands [`exports()`], [`big_stack_exports()`] to the
//! loader and [`services()`] to `nxkit_ipc::Ipc::new`.

mod commit;
mod files;
mod image;
mod ipc;
mod mount;
mod names;
mod ops;
mod result;
mod sdk;
mod sys;
mod table;
#[cfg(test)]
mod testutil;

use mount::Fs;
use nxkit_abi::export::Export;
use nxkit_abi::ipc::Named;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// What the service needs from the host, read from `game.json` and the run folder.
#[derive(Clone, Debug)]
pub struct Config {
    /// The title ID of the game. `IsSaveDataExisting` of this program is true.
    pub title_id: u64,
    /// The romfs folder of the run, with the base, update and DLC layers merged.
    pub romfs: PathBuf,
    /// The save folder of the account, for example `<run>/save/<title id>`. Its work and new folders are siblings.
    pub save: PathBuf,
    /// The folder of the temporary storage. The cache storages are siblings of it.
    pub temp: PathBuf,
    /// The folder of the add-on content, with one folder per index. `None` when the player has none.
    pub aoc: Option<PathBuf>,
    /// True when the game imports `nn::fs::MountSaveData` and `nn::fs::MountDeviceSaveData` both. The device save
    /// then has its own folder `<save>_device`. Otherwise both functions mount the account save.
    pub separate_device_save: bool,
}

/// The lookup of a symbol of the guest's own SDK code, by its mangled name. It returns the entry address of the
/// function the SDK has under that name, not the address of an export of this crate.
pub type GuestSym = fn(&str) -> Option<u64>;

static FS: OnceLock<Fs> = OnceLock::new();
static GUEST_SYM: OnceLock<GuestSym> = OnceLock::new();

/// Starts the service. The first call wins. A later call logs and changes nothing.
pub fn init(cfg: &Config) {
    if FS.set(Fs::new(cfg.clone())).is_err() {
        eprintln!("nxkit-svc-fs: init was called more than once, so the first configuration stays");
    }
}

/// Sets the lookup of guest SDK symbols. `nn::fs::Unmount` uses it to release a mount that the SDK holds in its own
/// table, such as a RAM disk. `nxkit-svc-fs` asks for `_ZN2nn2fs9IsMountedEPKc` (`bool IsMounted(const char *)`) and
/// `_ZN2nn2fs7UnmountEPKc` (`void Unmount(const char *)`), and calls what the lookup returns. The lookup must return
/// the functions of the SDK. A wrong address crashes the game. Without a lookup, `Unmount` of a name that is not a
/// mount of the host logs once and releases nothing. The first call wins.
pub fn set_guest_sym(f: GuestSym) {
    let _ = GUEST_SYM.set(f);
}

/// The guest address of the SDK function `name`, if the host set a lookup and it knows the name.
pub(crate) fn guest_sym(name: &str) -> Option<sys::GuestFn> {
    GUEST_SYM.get().and_then(|f| f(name)).map(sys::GuestFn)
}

/// The service, or `None` with a log when the host did not call [`init`]. The SDK functions fail then.
pub(crate) fn fs() -> Option<&'static Fs> {
    let fs = FS.get();
    if fs.is_none() {
        static NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !NOTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "[RED] nxkit-svc-fs: the game used the file system before init was called, so every call fails"
            );
        }
    }
    fs
}

/// The number of save commits since the start, for the exit report of the host.
pub fn commit_count() -> u64 {
    FS.get().map_or(0, Fs::commit_count)
}

/// The `nn::fs` functions that run on the stack of the calling guest thread.
pub fn exports() -> Vec<Export> {
    sdk::exports()
}

/// The `nn::fs` functions that the engine registers with `HLE_FORCE_BIG`. A path buffer is sized by `PATH_MAX` and an
/// open takes several KB of stack, while a game's save thread can have 8 KB, so the host runs these on a big stack.
pub fn big_stack_exports() -> Vec<Export> {
    sdk::big_exports()
}

/// The IPC services of this crate: `fsp-srv`. The root interface reaches the service at the time of a command, so
/// [`init`] may come before or after this call.
pub fn services() -> Vec<Named> {
    vec![Named {
        name: "fsp-srv",
        root: Arc::new(ipc::FspSrv::new()),
    }]
}
