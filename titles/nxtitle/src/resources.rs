//! Resource patches: an edit of one member of a game archive that the build applies to the app's RomFS.
//!
//! A title declares the patch as data: where the archive is, the digests of the exact bytes it was found on, and the
//! edit. The core reads, checks and writes the files. A build option names the patch by [`ResourcePatch::option`] and
//! gives the value. Files whose digests differ are not patched: the offsets hold only for the recorded bytes.

use nxb_model::Digest;

/// A zstd dictionary that a compressed archive was made with, kept in another compressed SARC of the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZstdDictionary {
    /// The compressed SARC holding the dictionary, from the RomFS root (`Pack/ZsDic.pack.zs`).
    pub archive: &'static str,
    /// The dictionary's member name in it (`pack.zsdic`).
    pub member: &'static str,
    /// The SHA-256 of the dictionary bytes.
    pub sha256: Digest,
}

/// What a patch changes in the member.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edit {
    /// Little-endian `f32` words, all set to the one value of the option.
    F32Words {
        /// Byte offsets in the member, 4-aligned and in increasing order.
        offsets: &'static [usize],
        /// The value must be greater than this.
        above: f32,
        /// The value must be at most this.
        up_to: f32,
    },
}

/// One resource patch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResourcePatch {
    /// The name of the build option (`resolution-scale`).
    pub option: &'static str,
    /// What the patch does, in one line.
    pub about: &'static str,
    /// The archive from the RomFS root: a zstd frame holding a SARC.
    pub archive: &'static str,
    /// The SHA-256 of the compressed archive file.
    pub archive_sha256: Digest,
    /// The dictionary the archive needs, if any.
    pub dictionary: Option<ZstdDictionary>,
    /// The member of the SARC that changes.
    pub member: &'static str,
    /// The SHA-256 of the original member bytes.
    pub member_sha256: Digest,
    /// The change.
    pub edit: Edit,
    /// The value without a build option. `None` leaves the game unchanged.
    pub default: Option<f32>,
}

impl Edit {
    /// Whether `v` is a value this edit takes.
    pub fn accepts(&self, v: f32) -> bool {
        match *self {
            Edit::F32Words { above, up_to, .. } => v.is_finite() && v > above && v <= up_to,
        }
    }
}
