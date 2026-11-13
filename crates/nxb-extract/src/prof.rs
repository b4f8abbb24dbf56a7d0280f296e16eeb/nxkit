//! Coarse time counters for the summary line (summed over threads, nanoseconds): where an extract spends its time.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// The counters.
pub struct Counters {
    /// `pread` of package bytes (block cache misses).
    pub pread: AtomicU64,
    /// AES decryption.
    pub decrypt: AtomicU64,
    /// IVFC block hashing.
    pub verify: AtomicU64,
    /// `pwrite` of image files.
    pub write: AtomicU64,
    /// SHA-256 per file in the workers.
    pub file_hash: AtomicU64,
    /// The fused split's scans.
    pub split: AtomicU64,
    /// The ordered image hashers (SHA-256 + SHA-1).
    pub image_hash: AtomicU64,
}

/// The process-wide counters.
pub static COUNTERS: Counters = Counters {
    pread: AtomicU64::new(0),
    decrypt: AtomicU64::new(0),
    verify: AtomicU64::new(0),
    write: AtomicU64::new(0),
    file_hash: AtomicU64::new(0),
    split: AtomicU64::new(0),
    image_hash: AtomicU64::new(0),
};

/// Runs `f` and records its duration when the host feature is enabled.
pub fn timed<T>(c: &AtomicU64, f: impl FnOnce() -> T) -> T {
    let t = cfg!(feature = "host").then(Instant::now);
    let r = f();
    if let Some(t) = t {
        c.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    r
}

/// `name core-seconds` pairs for the summary.
pub fn summary() -> String {
    let c = &COUNTERS;
    let s = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1e9;
    format!(
        "core-s: pread {:.1}, decrypt {:.1}, verify {:.1}, write {:.1}, file hash {:.1}, split {:.1}, image hash {:.1}",
        s(&c.pread),
        s(&c.decrypt),
        s(&c.verify),
        s(&c.write),
        s(&c.file_hash),
        s(&c.split),
        s(&c.image_hash)
    )
}
