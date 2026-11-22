//! Markers: what an engine leaves in a game that the core can find without knowing the engine.
//!
//! A family names its markers ([`crate::Family::markers`]). The core scans the modules and the RomFS file names once for
//! the markers of all families, and gives the hits to the families as libraries of the [`crate::TitleFacts`]. The id of a
//! marker is the name of the library it finds. The family decides from them ([`crate::Family::detect`]). A family that
//! only lists markers matches when one of them hit.

use crate::Evidence;

/// Where a marker is looked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerAt {
    /// A byte string in the read-only data of any module (a class name, a source path, a runtime's name).
    Rodata,
    /// A file in the RomFS, from its root. A pattern that ends with `/` matches a folder anywhere in a path, else the
    /// pattern is the end of the path.
    RomfsPath,
}

/// One trace of an engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker {
    /// The library that a hit names. Hits of equal ids are one library.
    pub id: &'static str,
    /// Where to look.
    pub at: MarkerAt,
    /// The text to find. Short names only: no text copied from a game.
    pub pattern: &'static str,
    /// Why the marker tells the engine.
    pub evidence: Evidence,
}

impl Marker {
    /// Whether a RomFS path (`Data/Managed/mscorlib.dll`) matches a marker of [`MarkerAt::RomfsPath`].
    pub fn matches_path(&self, path: &str) -> bool {
        if self.at != MarkerAt::RomfsPath {
            return false;
        }
        if self.pattern.ends_with('/') {
            path.starts_with(self.pattern) || path.contains(&format!("/{}", self.pattern))
        } else {
            path.ends_with(self.pattern)
        }
    }
}

/// The result of the engine detection of a game without a title module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detection {
    /// The id of the one family that matched, after a refinement settled several.
    pub family: Option<&'static str>,
    /// The ids of every family that matched, before a refinement: more than one without a family means no decision.
    pub candidates: Vec<&'static str>,
}
