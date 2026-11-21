//! Shared ids and digests for the nxkit workspace (layer L0, in the chain version V).
//!
//! - [`Digest`], [`Sha256`], [`sha256`]: SHA-256, own implementation, std only.
//! - [`Sha1`], [`DualHasher`], [`DualDigest`]: SHA-1 for `CodeResources`, and both digests in one pass.
//! - [`FileDigest`], [`HashingWriter`]: a payload's length and digest, computed once by the producer.
//! - [`ContentKey`]: canonical, versioned keys for the store and single-flight.
//! - [`ShaderKey`], [`TitleId`], [`VersionTag`], [`Target`], [`StageId`]: typed ids.
//!
//! No I/O beyond the generic `Write` wrapper. The only dependency is `nxb-sys` for the output-neutral hardware SHA-256
//! path (decisions.md W17).

mod digest;
mod ids;
mod key;
mod sha1;
mod sha256;

pub use digest::{Digest, FileDigest, HashingWriter, ParseError, to_hex};
pub use ids::{ShaderKey, StageId, Target, TitleId, VersionTag};
pub use key::{ContentKey, ContentKeyBuilder};
pub use sha1::{DualDigest, DualHasher, Sha1, Sha1Digest, sha1};
pub use sha256::{Backend, Sha256, sha256};
