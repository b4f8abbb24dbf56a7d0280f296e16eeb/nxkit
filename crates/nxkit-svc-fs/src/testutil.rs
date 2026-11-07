//! Scratch folders for the tests.

use crate::Config;
use std::path::PathBuf;
use nxkit_abi::guest;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A new empty folder under the target folder of the workspace. Each call gives a different one.
pub(crate) fn scratch(tag: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/nxkit-svc-fs-tests")
        .join(format!("{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A configuration with folders in a new scratch folder. The romfs folder exists, the others do not.
pub(crate) fn config(tag: &str) -> Config {
    let base = scratch(tag);
    std::fs::create_dir_all(base.join("romfs")).unwrap();
    Config {
        title_id: 0x0100_7EF0_0011_E000,
        romfs: base.join("romfs"),
        save: base.join("save").join("0100"),
        temp: base.join("temp"),
        aoc: Some(base.join("aoc")),
        separate_device_save: false,
    }
}

static GUEST: Mutex<()> = Mutex::new(());

/// A host buffer that stands for guest memory. The guest range is global, so one `Guest` lives at a time.
pub(crate) struct Guest {
    buf: Vec<u8>,
    _only: MutexGuard<'static, ()>,
}

impl Guest {
    /// A zeroed buffer of `len` bytes that becomes the guest range.
    pub(crate) fn new(len: usize) -> Guest {
        let only = GUEST.lock().unwrap_or_else(PoisonError::into_inner);
        let mut buf = vec![0u8; len];
        guest::set_range(buf.as_mut_ptr() as u64, len as u64);
        Guest { buf, _only: only }
    }

    /// The guest address of the first byte.
    pub(crate) fn addr(&self) -> u64 {
        self.buf.as_ptr() as u64
    }

    /// Stores `data` at the offset `off`.
    pub(crate) fn put(&self, off: u64, data: &[u8]) {
        guest::write(self.addr() + off, data).unwrap();
    }

    /// Stores `s` and a NUL at the offset `off` and returns the guest address.
    pub(crate) fn cstr(&self, off: u64, s: &str) -> u64 {
        self.put(off, s.as_bytes());
        self.put(off + s.len() as u64, &[0]);
        self.addr() + off
    }

    /// `len` bytes at the offset `off`.
    pub(crate) fn bytes(&self, off: u64, len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        guest::read(self.addr() + off, &mut v).unwrap();
        v
    }

    /// The `u64` at the offset `off`.
    pub(crate) fn u64_at(&self, off: u64) -> u64 {
        guest::read_u64(self.addr() + off).unwrap()
    }

    /// The `u32` at the offset `off`.
    pub(crate) fn u32_at(&self, off: u64) -> u32 {
        guest::read_u32(self.addr() + off).unwrap()
    }
}

/// The global service, started once for the tests of the SDK functions. The romfs holds `a.bin` (`0123456789`) and
/// `dir/b.bin` (`bb`), and the add-on content 1 holds `x.bin`.
pub(crate) fn global() -> &'static crate::mount::Fs {
    static START: Once = Once::new();
    START.call_once(|| {
        let cfg = config("global");
        std::fs::write(cfg.romfs.join("a.bin"), b"0123456789").unwrap();
        std::fs::create_dir(cfg.romfs.join("dir")).unwrap();
        std::fs::write(cfg.romfs.join("dir").join("b.bin"), b"bb").unwrap();
        let aoc = cfg.aoc.clone().unwrap().join("1");
        std::fs::create_dir_all(&aoc).unwrap();
        std::fs::write(aoc.join("x.bin"), b"x").unwrap();
        crate::init(&cfg);
    });
    crate::fs().unwrap()
}
