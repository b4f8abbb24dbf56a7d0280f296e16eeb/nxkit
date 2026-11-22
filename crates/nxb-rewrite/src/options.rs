//! The settings of one rewrite and the outcome of a pass of it. The rewrite and its parts (`cuts`, `engine`) share
//! them, so they sit below all three.

use crate::error::RewriteError;

/// Settings of one rewrite.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The module name (`main`, `sdk`, `subsdk0`, ...).
    pub name: String,
    /// `DM_PATCH_FRONT`: the front patch area size, when set by the caller.
    pub patch_front: Option<u64>,
    /// The game profile folder name (`DM_GAME`'s base name), or empty.
    pub profile: String,
    /// The scopes of the families' rule files (the engine family's id, then the graphics family's), whose
    /// function-matched rules apply after the profile's. Empty: the profile's rules only.
    pub layers: Vec<String>,
    /// `(SHA-256, path relative to dm/tools)` of the rewriter and patch files, for `build_src.sha256`.
    pub source_hashes: Vec<(String, String)>,
    /// The profile's `snowdrop_pools` words (the large, 512 KB and 64 KB ranges in MB), empty for the default.
    pub snowdrop_words: Vec<String>,
    /// Where the pool sizes come from, as the script's log says (`profile`, or `default, ...`), empty is `default, no DM_GAME`.
    pub snowdrop_from: String,
}

pub(crate) enum Step {
    Rerun(u64),
    Err(RewriteError),
}

impl From<RewriteError> for Step {
    fn from(e: RewriteError) -> Self {
        Step::Err(e)
    }
}
