//! Loads a patch module (W38): a `cdylib` with the symbols of `nxkit-patch-abi`.
//!
//! The library is copied to a path of its own before `dlopen`, so a rebuild of the same file is a new image and does
//! not hit the dyld cache of the old one. The copy is deleted once the library is mapped. The ABI version is checked
//! before the init function runs. The returned [`Entry`] holds a guard whose drop does the `dlclose`. The registry
//! gives the entry back only after no thread is inside a patch call, see `nxkit_patch_abi::registry`.

use super::*;
use nxkit_patch_abi as abi;
use nxkit_patch_abi::registry::Entry;

/// The longest log line of a patch that the host prints.
const MAX_LOG: usize = 8192;

/// A `dlopen` handle. The drop calls `dlclose`.
struct Handle(*mut c_void);

// SAFETY: a `dlopen` handle is a plain token that dyld accepts from any thread.
unsafe impl Send for Handle {}
// SAFETY: as for `Send`.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful `dlopen` and this is its only owner. The registry dropped the
        // entry only after no thread was inside a call of the patch. A library that has thread-local variables
        // stays mapped, dyld ignores the `dlclose` for it.
        unsafe { dlclose(self.0) };
    }
}

/// The log function that a patch gets.
extern "C" fn host_log(text: *const u8, len: usize) {
    if text.is_null() {
        return;
    }
    // SAFETY: the contract of `HostApi::log_raw` says that `text` points to `len` readable bytes. The length is
    // limited, so a wrong value cannot read far.
    let bytes = unsafe { std::slice::from_raw_parts(text, len.min(MAX_LOG)) };
    eprintln!("nxkit-patch: {}", String::from_utf8_lossy(bytes));
}

/// What the host gives every patch.
static HOST: abi::HostApi = abi::HostApi {
    abi_version: abi::ABI_VERSION,
    log_raw: host_log,
};

fn dl_message() -> String {
    // SAFETY: `dlerror` returns null or a C string that stays valid until the next dl call on this thread.
    let p = unsafe { dlerror() };
    if p.is_null() {
        return "unknown error".into();
    }
    // SAFETY: `p` is not null and points to a C string, see above.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// The address of the symbol `name` in the library, or null.
fn symbol(handle: &Handle, name: &str) -> *mut c_void {
    let Ok(c) = CString::new(name) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the handle is live and `c` is a valid C string.
    unsafe { dlsym(handle.0, c.as_ptr()) }
}

/// A path for the copy of the library that no earlier load used: the pid and the time in nanoseconds.
fn copy_path(src: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let stem = src
        .file_stem()
        .map_or_else(|| "patch".into(), |s| s.to_string_lossy().into_owned());
    // The folder of the control sockets, `$TMPDIR/nxkit` (`control::folder`). It is not imported: `control` uses `sys`.
    std::env::temp_dir()
        .join("nxkit")
        .join("patches")
        .join(format!("{}-{nanos}-{stem}.dylib", std::process::id()))
}

/// Loads the patch module at `path`. It is not active yet: install the entry in the registry.
pub fn load(path: &Path) -> std::result::Result<Entry, String> {
    let src = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let copy = copy_path(&src);
    if let Some(dir) = copy.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::copy(&src, &copy).map_err(|e| format!("copy {}: {e}", src.display()))?;
    let opened = open(&copy);
    // The mapping stays valid after the file is gone.
    let _ = std::fs::remove_file(&copy);
    let handle = opened?;
    let table = init(&handle)?;
    Ok(Entry {
        name: table.name().to_string(),
        path: src.display().to_string(),
        table,
        keep: Box::new(handle),
    })
}

fn open(copy: &Path) -> std::result::Result<Handle, String> {
    let c = CString::new(copy.to_string_lossy().as_bytes())
        .map_err(|_| format!("{} has a nul byte", copy.display()))?;
    // SAFETY: `c` is a valid C string.
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
    if h.is_null() {
        return Err(format!("dlopen: {}", dl_message()));
    }
    Ok(Handle(h))
}

/// Checks the ABI version and runs the init function. A failure drops the handle, which unloads the library.
fn init(handle: &Handle) -> std::result::Result<abi::PatchTable, String> {
    let version = symbol(handle, abi::SYMBOL_VERSION);
    if version.is_null() {
        return Err(format!("the library has no {}", abi::SYMBOL_VERSION));
    }
    // SAFETY: a patch exports this symbol with the signature of `AbiVersionFn` (the contract of the ABI).
    let version = unsafe { std::mem::transmute::<*mut c_void, abi::AbiVersionFn>(version) }();
    if version != abi::ABI_VERSION {
        return Err(format!(
            "the patch has ABI version {version}, the host has {}",
            abi::ABI_VERSION
        ));
    }
    let init = symbol(handle, abi::SYMBOL_INIT);
    if init.is_null() {
        return Err(format!("the library has no {}", abi::SYMBOL_INIT));
    }
    // SAFETY: a patch of the same ABI version exports this symbol with the signature of `InitFn`.
    let init = unsafe { std::mem::transmute::<*mut c_void, abi::InitFn>(init) };
    let mut table = abi::PatchTable::default();
    let status = init(&HOST, &mut table);
    if status != 0 {
        return Err(format!(
            "the patch refused to load (init returned {status})"
        ));
    }
    if table.abi_version != abi::ABI_VERSION {
        return Err("the table of the patch has another ABI version".into());
    }
    if table.name().is_empty() {
        return Err("the patch set no name".into());
    }
    Ok(table)
}
