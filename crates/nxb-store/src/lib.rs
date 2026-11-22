//! The content-addressed result store, format 1 (layer L2). It knows keys and blobs, never shaders.
//!
//! Layout under the root:
//!
//! ```text
//! FORMAT                              "nxb-store 1"
//! blobs/<2 hex>/<64 hex>              payload bytes, named by their SHA-256
//! entries/<2 hex>/<64 hex>.manifest   one entry per ContentKey: names -> (size, sha256), self-checksummed
//! quarantine/<time>-<pid>-<n>-<what>/ damaged blobs and entries, moved here, never deleted
//! tmp/                                temp files of writes in progress (same volume, so renames are atomic)
//! dirty/<token>-j<n>.journal          one per open store: blobs and entries being written (see below)
//! LOCK                                flock: shared for builds, exclusive for prune and migration
//! ```
//!
//! Rules (design-v2 10.2, 10.3, design-pipeline-rest 8):
//! - **Digests once.** A blob's digest is computed while its bytes are written ([`Store::put_bytes`],
//!   [`Store::put_reader`]). Manifests record it. Consumers take it from the manifest and never re-hash to learn it.
//! - **Durability.** Every file is written to a temp name (`O_EXCL`), `fsync`ed, renamed, then its directory is
//!   `fsync`ed. The manifest is written last, so an entry is visible only when all its blobs are durable.
//! - **Integrity.** [`Store::lookup`] checks the manifest checksum and every blob's size. [`Store::verify`] also
//!   re-hashes. Anything damaged moves to `quarantine/` and the lookup misses, so the work is redone.
//!
//! - **Locks.** [`Store::open`] holds a shared `flock` on `LOCK`, [`Store::open_exclusive`] an exclusive one.
//!   [`lock_output_dir`] guards a build's output directory.
//! - **Dirty marker.** Each open store journals what it is about to write under an exclusive lock, a clean drop deletes
//!   the journal. A journal whose lock is free belongs to a dead process: the next open re-hashes its blobs, verifies its
//!   entries (quarantine on damage), removes its temp files and deletes it.
//!
//! The title cache and its export format are in `cache` and `exchange` (`nxb cache`).
//!
//! Planned: the Python-compatible tree adapter (`legacy`), cross-process per-digest publish locks, prune, format 2
//! segment files.

mod cache;
mod dirty;
mod error;
mod exchange;
mod fsutil;
mod manifest;
mod store;

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod tests;

pub use cache::{CacheEntry, CacheKey, Filter, KEY_FILE, PruneReport};
pub use error::StoreError;
pub use exchange::{ExportReport, ImportReport};
pub use manifest::{Manifest, ManifestFile, validate_name};
pub use nxb_sys::lock::FileLock;
pub use store::{Durability, EntryWriter, Store, Verified, lock_output_dir};
